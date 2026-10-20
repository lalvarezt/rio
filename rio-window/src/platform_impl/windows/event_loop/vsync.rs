//! DwmFlush-driven vsync worker for window repaint continuations.
//!
//! Requests made during WM_PAINT rearm a per-window dirty flag after the
//! paint completes. Fresh input invalidates immediately; continuations use
//! this worker as their frame clock. Per composition cycle it
//! iterates the window registry and, for each window where
//! `dirty || should_present_after_input`, fires
//! `RedrawWindow(.., RDW_INVALIDATE)`. The app's `WM_PAINT` /
//! `RedrawRequested` path is unchanged.
//!
//! `should_present_after_input` keeps the loop firing for one
//! second *after a high-rate input burst* (≥ 60 events/sec over
//! a 100 ms window) even if the app never sets the dirty flag —
//! same gate macOS / Wayland / X11 use via `InputRateTracker`.
//! Single keystrokes / lone mouse events no longer force a
//! 1-second redraw storm.
//! With no dirty window or input sustain, the worker parks until new work arrives.
//!
//! When DWM is disabled, the monitor is unplugged, or under some
//! RDP modes, `DwmFlush` returns immediately. The 1 ms threshold
//! catches that and we fall back to `thread::sleep` at the queried
//! refresh interval. Same heuristic as zed's
//! `gpui_windows/src/vsync.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use crate::platform_impl::input_rate::InputRateTracker;

use windows_sys::Win32::Foundation::{HWND, S_OK};
use windows_sys::Win32::Graphics::Dwm::{
    DwmFlush, DwmGetCompositionTimingInfo, DWM_TIMING_INFO,
};
use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, RDW_INVALIDATE};
use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

/// State shared between the event loop, window-callback thread,
/// and the DwmFlush worker thread. Holds the per-window dirty-flag
/// registry plus the rate-gated post-input sustain tracker.
pub(crate) struct VSyncSharedState {
    /// HWND (as `usize` for `Hash`/`Eq`) → per-window dirty flag.
    /// WM_PAINT continuations set the flag; the worker reads
    /// and clears it on each tick.
    windows: RwLock<HashMap<usize, Arc<AtomicBool>>>,
    /// Rate-gated post-input sustain. Only sustained high-rate
    /// input (≥ 60 events/sec over 100 ms) keeps the worker fanning
    /// out redraws after the app stops marking windows dirty.
    /// `Mutex` because the worker thread and the window-message
    /// thread both touch this. See `platform_impl::input_rate`.
    input_rate_tracker: Mutex<InputRateTracker>,
    worker: OnceLock<Thread>,
}

impl VSyncSharedState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            windows: RwLock::new(HashMap::new()),
            input_rate_tracker: Mutex::new(InputRateTracker::new()),
            worker: OnceLock::new(),
        })
    }

    pub(crate) fn register_window(&self, hwnd: HWND) {
        let flag = Arc::new(AtomicBool::new(false));
        self.windows.write().unwrap().insert(hwnd as usize, flag);
    }

    pub(crate) fn unregister_window(&self, hwnd: HWND) {
        self.windows.write().unwrap().remove(&(hwnd as usize));
    }

    pub(crate) fn request_redraw(&self, hwnd: HWND) {
        if let Some(flag) = self.windows.read().unwrap().get(&(hwnd as usize)) {
            flag.store(true, Ordering::Release);
            self.wake();
        }
    }

    pub(crate) fn window_count(&self) -> usize {
        self.windows.read().unwrap().len()
    }

    #[inline]
    pub(crate) fn mark_input_received(&self) {
        let high_rate = {
            let mut tracker = self.input_rate_tracker.lock().unwrap();
            tracker.record_input();
            tracker.is_high_rate()
        };
        if high_rate {
            self.wake();
        }
    }

    #[inline]
    pub(crate) fn should_present_after_input(&self) -> bool {
        self.input_rate_tracker.lock().unwrap().is_high_rate()
    }

    fn has_work(&self) -> bool {
        self.should_present_after_input()
            || self
                .windows
                .read()
                .unwrap()
                .values()
                .any(|flag| flag.load(Ordering::Acquire))
    }

    fn wake(&self) {
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}

/// Owns the worker thread. Drop signals stop and joins.
pub(super) struct VSyncThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl VSyncThread {
    pub(super) fn spawn(state: Arc<VSyncSharedState>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();

        let handle = std::thread::Builder::new()
            .name("rio-window::vsync".to_owned())
            .spawn(move || {
                state
                    .worker
                    .set(std::thread::current())
                    .expect("vsync worker already registered");
                let provider = VSyncProvider::new();
                while !stop_worker.load(Ordering::Acquire) {
                    if !state.has_work() {
                        // unpark retains a token if work arrives before park,
                        // so an idle worker cannot lose a repaint request.
                        std::thread::park();
                        continue;
                    }
                    provider.wait_for_vsync();
                    if stop_worker.load(Ordering::Acquire) {
                        break;
                    }

                    let present_after_input = state.should_present_after_input();

                    // Snapshot HWND + flag pairs so we don't hold
                    // the registry lock across `RedrawWindow`.
                    let snapshot: Vec<(usize, Arc<AtomicBool>)> = state
                        .windows
                        .read()
                        .unwrap()
                        .iter()
                        .map(|(&hwnd, flag)| (hwnd, flag.clone()))
                        .collect();

                    for (hwnd_bits, flag) in snapshot {
                        let was_dirty = flag.swap(false, Ordering::AcqRel);
                        if !(was_dirty || present_after_input) {
                            continue;
                        }
                        let hwnd = hwnd_bits as HWND;
                        // SAFETY: `IsWindowVisible` and
                        // `RedrawWindow` are documented thread-safe.
                        unsafe {
                            if IsWindowVisible(hwnd) != 0 {
                                RedrawWindow(
                                    hwnd,
                                    std::ptr::null(),
                                    std::ptr::null_mut(),
                                    RDW_INVALIDATE,
                                );
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn rio-window vsync thread");

        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for VSyncThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            // Worker exits at the start of the next iteration after
            // the current DwmFlush returns (typically <16 ms).
            let _ = handle.join();
        }
    }
}

struct VSyncProvider {
    interval: Duration,
}

impl VSyncProvider {
    fn new() -> Self {
        let interval = query_dwm_interval().unwrap_or(DEFAULT_VSYNC_INTERVAL);
        Self { interval }
    }

    fn wait_for_vsync(&self) {
        let start = Instant::now();
        let hr = unsafe { DwmFlush() };
        let elapsed = start.elapsed();
        if hr != S_OK || elapsed < VSYNC_INTERVAL_THRESHOLD {
            std::thread::sleep(self.interval);
        }
    }
}

fn query_dwm_interval() -> Option<Duration> {
    let mut frequency: i64 = 0;
    if unsafe { QueryPerformanceFrequency(&mut frequency) } == 0 || frequency <= 0 {
        return None;
    }
    let qpc_per_second = frequency as u64;

    let mut info: DWM_TIMING_INFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<DWM_TIMING_INFO>() as u32;
    if unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut info) } != S_OK {
        return None;
    }

    let interval = ticks_to_duration(info.qpcRefreshPeriod, qpc_per_second);
    if interval >= VSYNC_INTERVAL_THRESHOLD {
        return Some(interval);
    }
    if info.rateRefresh.uiNumerator == 0 {
        return None;
    }
    Some(ticks_to_duration(
        info.rateRefresh.uiDenominator as u64,
        info.rateRefresh.uiNumerator as u64,
    ))
}

#[inline]
fn ticks_to_duration(counts: u64, ticks_per_second: u64) -> Duration {
    Duration::from_secs_f64(counts as f64 / ticks_per_second as f64)
}

#[cfg(test)]
mod tests {
    use super::ticks_to_duration;
    use std::time::Duration;

    #[test]
    fn refresh_intervals_support_qpc_and_rational_rates() {
        for (ticks, frequency, expected_nanos) in [
            (166_667, 10_000_000, 16_666_700),
            (1, 60, 16_666_667),
            (1_001, 60_000, 16_683_333),
            (1, 144, 6_944_444),
        ] {
            assert_eq!(
                ticks_to_duration(ticks, frequency),
                Duration::from_nanos(expected_nanos),
            );
        }
    }
}
