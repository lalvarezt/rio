use super::*;

#[test]
#[ignore = "performance storage probe: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_text_cache() {
    let fonts = FontLibrary::default();
    let mut text = Text::new(&fonts);
    text.init_cpu();
    for index in 0..4096 {
        assert!(
            text.measure(
                &format!("~/project-{index:04}/src/main.rs"),
                &DrawOpts::default()
            ) > 0.0
        );
    }
    println!(
        "RIO_COUNTER {{\"case\":\"ui_text/title_churn\",\"retained_shapes\":{}}}",
        text.shape_cache.len()
    );
}
