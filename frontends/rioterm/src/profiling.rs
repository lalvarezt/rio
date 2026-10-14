//! Timings for the opt-in profiling executable. File I/O happens only at exit.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Capture {
    start: Instant,
    ready: Option<Instant>,
    ready_routes: BTreeSet<usize>,
    target_tabs: usize,
    first_frame: Option<Instant>,
    shell_frame: Option<Instant>,
    frames: usize,
    target_frames: usize,
    samples: BTreeMap<&'static str, Vec<f64>>,
    active: Vec<&'static str>,
}

static CAPTURE: OnceLock<Mutex<Capture>> = OnceLock::new();
static DIAGNOSTICS: AtomicBool = AtomicBool::new(false);

pub fn init() {
    CAPTURE.get_or_init(|| {
        Mutex::new(Capture {
            start: Instant::now(),
            ready: None,
            ready_routes: BTreeSet::new(),
            target_tabs: tab_count() * window_count(),
            first_frame: None,
            shell_frame: None,
            frames: 0,
            target_frames: std::env::var("RIO_PROFILE_FRAMES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            samples: BTreeMap::new(),
            active: Vec::new(),
        })
    });
    if std::env::var_os("RIO_PROFILE_WATCHDOG").is_some() {
        DIAGNOSTICS.store(true, Ordering::Relaxed);
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(10));
            write_report(output_path().with_extension("watchdog.json"));
        });
    }
}

pub struct Span(&'static str, Instant);

pub fn span(name: &'static str) -> Span {
    active(name, true);
    Span(name, Instant::now())
}

impl Drop for Span {
    fn drop(&mut self) {
        let elapsed = self.1.elapsed().as_secs_f64() * 1000.0;
        record(self.0, elapsed);
        active(self.0, false);
    }
}

fn active(name: &'static str, entering: bool) {
    if !DIAGNOSTICS.load(Ordering::Relaxed) {
        return;
    }
    let mut capture = CAPTURE.get().unwrap().lock().unwrap();
    if entering {
        capture.active.push(name);
    } else if let Some(index) = capture.active.iter().rposition(|n| *n == name) {
        capture.active.remove(index);
    }
}

fn record(name: &'static str, elapsed: f64) {
    if let Some(capture) = CAPTURE.get() {
        let mut capture = capture.lock().unwrap();
        let samples = capture.samples.entry(name).or_default();
        if samples.len() < 100_000 {
            samples.push(elapsed);
        }
    }
}

/// Collect the targeted startup spans emitted by renderer dependencies.
pub struct TimingLayer;

impl<S> tracing_subscriber::Layer<S> for TimingLayer
where
    S: tracing::Subscriber
        + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_new_span(
        &self,
        _attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            active(span.metadata().name(), true);
            span.extensions_mut().insert(Instant::now());
        }
    }

    fn on_close(
        &self,
        id: tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(&id) {
            if let Some(start) = span.extensions().get::<Instant>() {
                record(
                    span.metadata().name(),
                    start.elapsed().as_secs_f64() * 1000.0,
                );
                active(span.metadata().name(), false);
            }
        }
    }
}

pub fn tab_count() -> usize {
    std::env::var("RIO_PROFILE_TABS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .clamp(1, 16)
}

pub fn window_count() -> usize {
    std::env::var("RIO_PROFILE_WINDOWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .clamp(1, 4)
}

pub fn shell_title(route: usize, title: Option<&str>) {
    if title.is_some_and(|title| title.contains("RIO_PROFILE_READY")) {
        let mut capture = CAPTURE.get().unwrap().lock().unwrap();
        capture.ready_routes.insert(route);
        if capture.ready_routes.len() == capture.target_tabs && capture.ready.is_none() {
            capture.ready = Some(Instant::now());
        }
    }
}

pub fn frame() {
    let mut capture = CAPTURE.get().unwrap().lock().unwrap();
    capture.first_frame.get_or_insert_with(Instant::now);
    if capture.ready.is_some() {
        capture.shell_frame.get_or_insert_with(Instant::now);
        capture.frames += 1;
    }
}

pub fn needs_frame() -> bool {
    let capture = CAPTURE.get().unwrap().lock().unwrap();
    capture.ready.is_some()
        && (capture.shell_frame.is_none() || capture.frames < capture.target_frames)
}

pub fn finished() -> bool {
    let capture = CAPTURE.get().unwrap().lock().unwrap();
    capture.start.elapsed() >= Duration::from_secs(20)
        || capture
            .ready
            .zip(capture.shell_frame)
            .is_some_and(|(ready, frame)| {
                ready.max(frame).elapsed() >= Duration::from_secs(2)
                    && capture.frames >= capture.target_frames
            })
}

fn output_path() -> std::path::PathBuf {
    std::env::var_os("RIO_PROFILE_OUTPUT")
        .unwrap_or_else(|| "rio-profile.json".into())
        .into()
}

pub fn report() {
    write_report(output_path());
}

fn write_report(path: std::path::PathBuf) {
    let mut capture = CAPTURE.get().unwrap().lock().unwrap().clone();
    let mut file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let ready = capture
        .ready
        .map(|t| (t - capture.start).as_secs_f64() * 1000.0);
    let first_frame = capture
        .first_frame
        .map(|t| (t - capture.start).as_secs_f64() * 1000.0);
    let shell_frame = capture
        .shell_frame
        .map(|t| (t - capture.start).as_secs_f64() * 1000.0);
    let number =
        |value: Option<f64>| value.map_or("null".to_owned(), |v| format!("{v:.6}"));
    writeln!(
        file,
        "{{\"active\":{:?},\"ready_tabs\":{},\"target_tabs\":{},\"frames\":{},\"target_frames\":{},\"shell_ready_ms\":{},\"first_frame_ms\":{},\"shell_frame_ms\":{},\"total_ms\":{:.6},\"metrics\":{{",
        capture.active,
        capture.ready_routes.len(),
        capture.target_tabs,
        capture.frames,
        capture.target_frames,
        number(ready),
        number(first_frame),
        number(shell_frame),
        capture.start.elapsed().as_secs_f64() * 1000.0
    )
    .unwrap();
    let mut separator = "";
    for (name, samples) in &mut capture.samples {
        let first = samples[0];
        samples.sort_by(f64::total_cmp);
        let n = samples.len();
        let median = (samples[(n - 1) / 2] + samples[n / 2]) / 2.0;
        write!(file, "{separator}\"{name}\":{{\"count\":{n},\"total_ms\":{:.6},\"median_ms\":{:.6},\"p95_ms\":{:.6},\"max_ms\":{:.6},\"first_ms\":{first:.6}}}",
            samples.iter().sum::<f64>(), median, samples[(n as f64 * 0.95).ceil() as usize - 1], samples[n - 1]).unwrap();
        separator = ",\n";
    }
    writeln!(file, "}}}}").unwrap();
}
