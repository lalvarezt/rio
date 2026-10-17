use super::*;
use crate::performance::measure;
use std::hint::black_box;

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_logos() {
    let logo = &LOGOS[0];
    for size in [24, 48] {
        measure(&format!("icons/svg_raster/{size}"), 1, || {
            let pixels = raster(logo, black_box(size));
            assert_eq!(pixels.len(), usize::from(size) * usize::from(size) * 4);
            black_box(pixels).len() as u64
        });
    }
}
