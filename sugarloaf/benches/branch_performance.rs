#[path = "../../misc/benchmarks/support.rs"]
mod support;

use std::hint::black_box;
use sugarloaf::{
    font::FontLibrary,
    text::{DrawOpts, Text},
};

fn main() {
    let fonts = FontLibrary::default();
    let opts = DrawOpts::default();
    let mut text = Text::new(&fonts);
    text.init_cpu();
    for (name, label) in [
        ("ui_text/cached_short", "home/lalvarezt"),
        (
            "ui_text/cached_long",
            "~/projects/rio/frontends/rioterm/src/renderer/island/tabs.rs",
        ),
        ("ui_text/cached_unicode", "日本語 한글 terminal"),
    ] {
        assert!(text.measure(label, &opts) > 0.0);
        support::measure(name, label.len(), || {
            text.clear();
            let width = text.draw(12.0, 10.0, black_box(label), &opts);
            black_box(text.instances());
            width.to_bits() as u64
        });
        support::measure(&name.replace("cached_", "measure_"), label.len(), || {
            black_box(text.measure(black_box(label), &opts)).to_bits() as u64
        });
        // Keep the raster check valid even when a different case is selected.
        text.clear();
        text.draw(12.0, 10.0, label, &opts);
        let mut pixels = vec![0u32; 1024 * 64];
        text.render_cpu(&mut pixels, 1024, 64);
        assert!(pixels.iter().any(|pixel| *pixel != 0));
        support::check(
            name,
            support::hash(pixels.iter().flat_map(|pixel| pixel.to_le_bytes())),
        );
    }
    let labels: Vec<_> = (0..4096)
        .map(|n| format!("~/project-{n:04}/src/main.rs"))
        .collect();
    let mut index = 0;
    support::measure("ui_text/title_churn", 1, || {
        text.clear();
        index = (index + 1) % labels.len();
        black_box(text.measure(&labels[index], &opts)).to_bits() as u64
    });
}
