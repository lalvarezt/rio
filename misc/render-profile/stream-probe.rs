// Standalone merge-validation probe. See specs/upstream-merge-2026-11-15.md.
use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::grid::Dimensions;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{TerminalDamage, VoidListener, WindowId};
use rio_vt::performer::handler::Processor;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

// The pre-merge fork always handles status. Upstream adds an explicit opt-in.
trait EnableProgramStatus {
    fn set_program_status_enabled(&mut self, _enabled: bool) {}
}
impl<T: rio_vt::event::EventListener> EnableProgramStatus for Crosswords<T> {}

struct Allocator;
static COUNT: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(layout.size(), Relaxed);
        }
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNT.load(Relaxed) {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(size, Relaxed);
        }
        System.realloc(ptr, layout, size)
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let chunk: usize = args.get(2).ok_or("missing chunk size")?.parse()?;
    let snapshot_bytes: usize =
        args.get(3).ok_or("missing snapshot interval")?.parse()?;
    if chunk == 0 || chunk > 65536 {
        return Err("chunk size must be 1..=65536".into());
    }
    let mut terminal = Crosswords::new(
        CrosswordsSize::new(120, 80),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        10_000,
    );
    terminal.set_program_status_enabled(true);
    let mut processor = Processor::default();
    let mut rows = Vec::new();
    let mut styles = Vec::new();
    let mut extras = Default::default();
    let mut file = std::fs::File::open(args.get(1).ok_or("missing input")?)?;
    let mut buffer = [0; 65536];
    let (mut bytes, mut since_snapshot, mut snapshots) = (0, 0, 0);
    COUNT.store(args.get(4).is_some_and(|s| s == "alloc"), Relaxed);
    let start = Instant::now();
    loop {
        let n = file.read(&mut buffer[..chunk])?;
        if n == 0 {
            break;
        }
        processor.advance(&mut terminal, &buffer[..n]);
        bytes += n;
        since_snapshot += n;
        if snapshot_bytes != 0 && since_snapshot >= snapshot_bytes {
            terminal.snapshot_visible(
                &TerminalDamage::Partial,
                &mut rows,
                &mut styles,
                &mut extras,
            );
            terminal.reset_damage();
            std::hint::black_box((&rows, &styles, &extras));
            since_snapshot = 0;
            snapshots += 1;
        }
    }
    let ns = start.elapsed().as_nanos();
    COUNT.store(false, Relaxed);
    let allocations = ALLOCS.load(Relaxed);
    let allocated_bytes = BYTES.load(Relaxed);
    let hash = terminal
        .grid
        .display_iter()
        .fold(0xcbf29ce484222325_u64, |h, cell| {
            (h ^ u64::from(cell.square.c() as u32)).wrapping_mul(0x100000001b3)
        });
    let rss_kib = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse::<usize>().ok())
        })
        .unwrap_or(0);
    println!("{{\"rss_kib\":{rss_kib},\"ns\":{ns},\"bytes\":{bytes},\"snapshots\":{snapshots},\"hash\":{hash},\"allocations\":{allocations},\"allocated_bytes\":{allocated_bytes},\"history\":{},\"cursor\":\"{:?}\"}}", terminal.grid.history_size(), terminal.grid.cursor.pos);
    Ok(())
}
