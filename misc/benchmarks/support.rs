//! Shared measurement code for the branch benchmarks. No production code uses it.
// Each benchmark executable imports only the helpers it needs.
#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::{Duration, Instant};

struct Allocator;
static COUNT: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNT.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size() as u64, Relaxed);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNT.load(Relaxed) {
            ALLOCATIONS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size as u64, Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}

fn setting(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().expect("benchmark setting must be an integer"))
        .unwrap_or(default)
}

pub fn selected(name: &str) -> bool {
    std::env::var("RIO_BENCH_FILTER")
        .map(|filter| name.contains(&filter))
        .unwrap_or(true)
}

/// Print every sample, not just its median. Allocation counting is a separate
/// warmed pass so atomic increments do not contaminate the timing samples.
pub fn measure(name: &str, units: usize, mut operation: impl FnMut() -> u64) {
    if !selected(name) {
        return;
    }
    let samples = setting("RIO_BENCH_SAMPLES", 11);
    let duration = Duration::from_millis(setting("RIO_BENCH_MS", 100));
    assert!(samples >= 3 && !duration.is_zero());
    let mut iterations = 1u64;
    loop {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(operation());
        }
        if start.elapsed() >= duration / 4 {
            break;
        }
        iterations = iterations.checked_mul(2).expect("iteration overflow");
    }
    iterations = iterations.checked_mul(4).expect("iteration overflow");
    for sample in 0..samples {
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(operation());
        }
        let ns = start.elapsed().as_nanos() as f64 / iterations as f64;
        println!("RIO_BENCH {{\"case\":\"{name}\",\"sample\":{sample},\"iterations\":{iterations},\"units\":{units},\"ns\":{ns}}}");
    }
    ALLOCATIONS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNT.store(true, Relaxed);
    for _ in 0..16 {
        black_box(operation());
    }
    COUNT.store(false, Relaxed);
    let allocations = ALLOCATIONS.load(Relaxed) as f64 / 16.0;
    let bytes = BYTES.load(Relaxed) as f64 / 16.0;
    println!("RIO_ALLOC {{\"case\":\"{name}\",\"allocations\":{allocations},\"bytes\":{bytes}}}");
}

pub fn check(name: &str, value: u64) {
    println!("RIO_CHECK {{\"case\":\"{name}\",\"value\":\"{value:016x}\"}}");
}

/// Time a state transition once per fresh fixture. Setup and destruction are
/// outside the timer, so atlas growth cannot become a warmed insertion loop.
pub fn measure_cold<T>(
    name: &str,
    units: usize,
    mut setup: impl FnMut() -> T,
    mut operation: impl FnMut(&mut T) -> u64,
) {
    if !selected(name) {
        return;
    }
    let samples = setting("RIO_BENCH_SAMPLES", 11);
    assert!(samples >= 3);
    for sample in 0..samples {
        let mut fixture = setup();
        let start = Instant::now();
        black_box(operation(&mut fixture));
        let ns = start.elapsed().as_nanos();
        println!("RIO_BENCH {{\"case\":\"{name}\",\"sample\":{sample},\"iterations\":1,\"units\":{units},\"ns\":{ns},\"cold_transition\":true}}");
    }
    let mut fixture = setup();
    ALLOCATIONS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    COUNT.store(true, Relaxed);
    black_box(operation(&mut fixture));
    COUNT.store(false, Relaxed);
    println!(
        "RIO_ALLOC {{\"case\":\"{name}\",\"allocations\":{},\"bytes\":{}}}",
        ALLOCATIONS.load(Relaxed),
        BYTES.load(Relaxed)
    );
}

pub fn hash(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
