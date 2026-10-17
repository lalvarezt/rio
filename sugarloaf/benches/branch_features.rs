#[path = "../../misc/benchmarks/support.rs"]
mod support;

use std::cell::Cell;
use sugarloaf::{font::FontLibrary, text::Text};

fn main() {
    let fonts = FontLibrary::default();
    let mut text = Text::new(&fonts);
    text.init_cpu();
    let rasters = Cell::new(0);
    let raster = |size: u16| {
        rasters.set(rasters.get() + 1);
        vec![255u8; usize::from(size) * usize::from(size) * 4]
    };
    assert!(text.draw_icon("benchmark", 20.0, 20.0, 24.0, [255; 4], raster));
    support::measure("icons/atlas_hit", 1, || {
        text.clear();
        assert!(text.draw_icon("benchmark", 20.0, 20.0, 24.0, [255; 4], raster));
        text.instance_count() as u64
    });
    assert_eq!(rasters.get(), 1, "cached artwork must not rasterize again");
    let failures = Cell::new(0);
    let invalid = |_| {
        failures.set(failures.get() + 1);
        Vec::new()
    };
    assert!(!text.draw_icon("invalid", 20.0, 20.0, 24.0, [255; 4], invalid));
    support::measure("icons/failed_upload_hit", 1, || {
        assert!(!text.draw_icon("invalid", 20.0, 20.0, 24.0, [255; 4], invalid));
        0
    });
    assert_eq!(
        failures.get(),
        1,
        "failed immutable artwork must not retry every frame"
    );
}
