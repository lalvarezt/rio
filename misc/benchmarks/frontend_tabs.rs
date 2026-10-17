use super::*;
use crate::performance::measure;

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_animation() {
    for (kind, animation, interval) in [
        ("none", Animation::None, 1.0 / 60.0),
        ("medium", Animation::Medium, 1.0 / 30.0),
        ("full", Animation::Full, 1.0 / 60.0),
    ] {
        let mut frames = 0;
        measure(&format!("animation/{kind}"), 1, || {
            let mut position = Position::new(20.0);
            position.target(440.0);
            frames = 0;
            while position.advance(std::hint::black_box(interval), 1.0, animation) {
                frames += 1;
                assert!(frames < 300, "motion must settle");
            }
            assert_eq!(position.value(), 440.0);
            frames
        });
        println!("RIO_COUNTER {{\"case\":\"animation/{kind}\",\"steps\":{frames}}}");
    }
    let tabs = Tabs::default();
    measure("animation/idle_deadline", 1, || {
        assert!(std::hint::black_box(&tabs)
            .next_frame_in(1.0, None)
            .is_none());
        0
    });
}
