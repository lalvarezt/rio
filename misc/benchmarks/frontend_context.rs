use super::*;
use crate::performance::measure;
use rio_backend::crosswords::pos::{Column, Line, Pos};
use rio_backend::event::{TerminalDamage, VoidListener};

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_selection() {
    let dimension = ContextDimension {
        columns: 160,
        lines: 60,
        ..Default::default()
    };
    let mut context =
        create_dead_context(VoidListener, WindowId::from(0), 0, 0, dimension);
    let mut rows = Vec::new();
    let mut styles = Vec::new();
    let mut extras = Default::default();
    context.terminal.lock().snapshot_visible(
        &TerminalDamage::Full,
        &mut rows,
        &mut styles,
        &mut extras,
    );
    let mut index = 0;
    let mut dirty_rows = 0;
    measure("selection/three_rows_snapshot", 60, || {
        for row in &mut rows {
            row.dirty = false;
        }
        index ^= 1;
        let selection = SelectionRange::new(
            Pos::new(Line(2 + index), Column(5)),
            Pos::new(Line(4 + index), Column(35)),
            false,
        );
        context.set_selection(Some(selection));
        let damage = context
            .renderable_content
            .pending_update
            .take_terminal_damage()
            .unwrap();
        context.terminal.lock().snapshot_visible(
            &damage,
            &mut rows,
            &mut styles,
            &mut extras,
        );
        context.renderable_content.pending_update.reset();
        dirty_rows = rows.iter().filter(|row| row.dirty).count();
        dirty_rows as u64
    });
    println!("RIO_COUNTER {{\"case\":\"selection/three_rows_snapshot\",\"dirty_rows\":{dirty_rows}}}");
}
