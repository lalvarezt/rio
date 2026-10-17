use super::*;
use crate::performance::measure;
use std::hint::black_box;

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_catalog() {
    let library = Library::default();
    for (kind, title) in [
        ("local", "nvim"),
        ("remote", "ssh"),
        ("windows_path", "C:\\Program Files\\Git\\bin\\git.exe"),
        ("versioned", "python3.13"),
    ] {
        measure(&format!("icons/catalog/{kind}"), 1, || {
            black_box(library.resolve(black_box(title), title, None, false));
            0
        });
    }
}
