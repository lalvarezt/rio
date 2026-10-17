use super::*;
use crate::performance::measure;

fn set_query(overlay: &mut SearchOverlay, value: &str) {
    let overlay = std::hint::black_box(overlay);
    let value = std::hint::black_box(value);
    overlay.set_active_search(Some(value));
}

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_search() {
    let mut overlay = SearchOverlay::default();
    set_query(&mut overlay, "needle");
    measure("search/unchanged_query", 1, || {
        set_query(&mut overlay, "needle");
        overlay.is_active() as u64
    });
    let mut index = 0;
    measure("search/changing_query", 1, || {
        index ^= 1;
        set_query(&mut overlay, if index == 0 { "needle" } else { "thread" });
        overlay.is_active() as u64
    });
}
