use super::*;
use crate::performance::measure;
use rio_backend::{
    ansi::CursorShape,
    crosswords::{Crosswords, CrosswordsSize},
    event::{VoidListener, WindowId},
    performer::handler::Processor,
};

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_status() {
    let mut terminal = Crosswords::new(
        CrosswordsSize::new(160, 60),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        0,
    );
    let mut parser = Processor::default();
    parser.advance(&mut terminal, b"\x1b]7501;state=working:app=codex\x1b\\");
    let mut island = Island::new([0.6; 4], [0.9; 4], false, 440.0);
    island.sync_program_status(0, &terminal.program_status);
    measure("status/idle_deadline", 1, || {
        assert!(std::hint::black_box(&island)
            .next_frame_in(1.0, std::time::Duration::from_millis(16))
            .is_none());
        0
    });
    measure("status/revision_hit", 1, || {
        std::hint::black_box(&mut island)
            .sync_program_status(0, std::hint::black_box(&terminal.program_status));
        terminal.program_status.revision()
    });
    let mut progress = 0;
    measure("status/report_and_sync", 1, || {
        progress = (progress + 1) % 101;
        let sequence =
            format!("\x1b]7501;state=working:app=codex:progress={progress}\x1b\\");
        parser.advance(&mut terminal, sequence.as_bytes());
        island.sync_program_status(0, &terminal.program_status);
        terminal.program_status.revision()
    });
}
