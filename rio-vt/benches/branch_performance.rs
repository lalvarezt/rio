#[path = "../../misc/benchmarks/support.rs"]
mod support;

use rio_vt::ansi::CursorShape;
use rio_vt::crosswords::{Crosswords, CrosswordsSize};
use rio_vt::event::{TerminalDamage, VoidListener, WindowId};
use rio_vt::performer::handler::Processor;
use std::hint::black_box;

fn terminal() -> Crosswords<VoidListener> {
    Crosswords::new(
        CrosswordsSize::new(160, 60),
        CursorShape::Block,
        VoidListener,
        WindowId::from(0),
        0,
        2000,
    )
}

fn input() {
    for (name, line) in [
        ("input/ascii", "the quick brown fox jumps over the lazy dog 0123456789\r\n"),
        ("input/unicode", "日本語 한글 λ α Ж e\u{301} 👩\u{200d}💻 ╔═╗\r\n"),
        ("input/mixed", "日本語 the quick brown fox jumps over the lazy dog 0123456789 abcdefghijklmnopqrstuvwxyz\r\n"),
        ("input/ansi", "\x1b[1;32mgreen\x1b[0m \x1b[38;2;115;230;45mtruecolor\x1b[0m\r\n"),
        ("input/csi", "\x1b[2;10H\x1b[K\x1b[0mtext\x1b[1;1H\x1b[mword\r\n"),
    ] {
        if !support::selected(name) { continue; }
        let bytes = line.repeat(262_144 / line.len()).into_bytes();
        let mut term = terminal();
        let mut processor = Processor::default();
        support::measure(name, bytes.len(), || {
            processor.advance(&mut term, black_box(&bytes));
            black_box(&term);
            0
        });
        // A fresh replay keeps the check independent of calibrated iteration count.
        let mut term = terminal();
        Processor::default().advance(&mut term, &bytes);
        let mut rows = Vec::new();
        term.fill_visible_rows(&mut rows);
        support::check(name, support::hash(rows.iter().flat_map(|row| {
            row.inner.iter().flat_map(|cell| (cell.c() as u32).to_le_bytes())
        })));
    }
}

fn snapshots() {
    for styled in [false, true] {
        let kind = if styled { "styled" } else { "plain" };
        let mut term = terminal();
        let mut processor = Processor::default();
        let mut fixture = String::new();
        for row in 1..=60 {
            fixture.push_str(&format!("\x1b[{row};1H"));
            if styled {
                fixture.push_str("\x1b[1;32m");
            }
            fixture.push_str(
                &"snapshot default styles and retained row storage ".repeat(4)[..160],
            );
        }
        processor.advance(&mut term, fixture.as_bytes());
        let mut rows = Vec::new();
        let mut styles = Vec::new();
        let mut extras = Default::default();
        term.snapshot_visible(&TerminalDamage::Full, &mut rows, &mut styles, &mut extras);
        for (operation, damage) in [
            ("full", TerminalDamage::Full),
            ("one_row", TerminalDamage::Partial),
            ("cursor", TerminalDamage::CursorOnly),
        ] {
            let name = format!("snapshot/{kind}/{operation}");
            let mut sequence = 0;
            support::measure(&name, 160 * 60, || {
                if operation == "one_row" {
                    processor.advance(
                        &mut term,
                        if sequence & 1 == 0 {
                            b"\x1b[2;1HA"
                        } else {
                            b"\x1b[2;1HB"
                        },
                    );
                    sequence += 1;
                }
                term.snapshot_visible(
                    black_box(&damage),
                    &mut rows,
                    &mut styles,
                    &mut extras,
                );
                black_box((&rows, &styles, &extras));
                rows.len() as u64
            });
            assert_eq!(rows.len(), 60);
            assert_eq!(rows[0].inner.len(), 160);
        }
    }
}

fn status_limit() {
    let name = "status/oversized_sequence";
    if !support::selected(name) {
        return;
    }
    let input = format!("\x1b]7501;state=working:app={}\x1b\\", "a".repeat(32768));
    support::measure(name, input.len(), || {
        let mut term = terminal();
        Processor::default().advance(&mut term, input.as_bytes());
        black_box(term);
        0
    });
}

fn main() {
    input();
    snapshots();
    status_limit();
}
