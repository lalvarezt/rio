use super::*;
use crate::performance::{check, measure};
use std::hint::black_box;

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_palette() {
    let targets: Vec<_> = (0..1024)
        .map(|i| format!("Example Font Family {i:04} Regular"))
        .collect();
    for unicode in [false, true] {
        let labels: Vec<_> = targets
            .iter()
            .map(|target| {
                if unicode {
                    format!("日本語 {target}")
                } else {
                    target.clone()
                }
            })
            .collect();
        let name = if unicode {
            "palette/score_unicode"
        } else {
            "palette/score_ascii"
        };
        measure(name, labels.len(), || {
            labels
                .iter()
                .filter_map(|target| score(black_box("font 00"), black_box(target)))
                .map(|score| score as u64)
                .sum()
        });
    }
    let mut palette = CommandPalette::new();
    palette.enter_fonts_mode(targets);
    palette.set_query("font 00".into());
    assert!(palette.get_selected_font().is_some());
    measure("palette/stable_results", 1024, || {
        black_box(palette.get_selected_font()).map_or(0, |font| font.len() as u64)
    });
    measure("palette/navigation", 1024, || {
        palette.move_selection_down();
        palette.move_selection_up();
        black_box(palette.get_selected_font()).map_or(0, |font| font.len() as u64)
    });
    let mut index = 0;
    measure("palette/edit_query", 1024, || {
        index ^= 1;
        palette.set_query(if index == 0 { "font 00" } else { "font 01" }.into());
        black_box(palette.get_selected_font()).map_or(0, |font| font.len() as u64)
    });
    palette.set_query("font 00".into());
    check(
        "palette/selected",
        crate::performance::hash(palette.get_selected_font().unwrap().bytes()),
    );
    let mut append = CommandPalette::new();
    append.enter_fonts_mode(vec!["Font Family 0000".into(), "Font Family 0001".into()]);
    append.set_query("font ".into());
    measure("palette/append_query", 1, || {
        // Reset only the fixture string. append_query performs the real edit,
        // invalidation, and filtering on both revisions.
        append.query.truncate(5);
        assert!(append.append_query("0"));
        std::hint::black_box(append.get_selected_font())
            .map_or(0, |font| font.len() as u64)
    });
}
