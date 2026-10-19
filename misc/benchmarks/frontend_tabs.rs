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

#[test]
#[ignore = "performance benchmark: run with --ignored --nocapture --test-threads=1"]
fn branch_bench_geometry() {
    use rio_backend::config::layout::Margin;
    use rio_backend::config::navigation::{Navigation, Tabs as TabOverrides};
    for (name, style) in [("pill", TabStyle::Pill), ("square", TabStyle::Square)] {
        let mut island = Island::new([0.6; 4], [0.9; 4], false, 440.0);
        for custom in [false, true] {
            let mut navigation = Navigation {
                tab_style: style,
                ..Default::default()
            };
            if custom {
                navigation.tabs = TabOverrides {
                    height: Some(80.0),
                    margin: Some(Margin::new(12.0, 20.0, 10.0, 18.0)),
                    gap: Some(8.0),
                    radius: Some(12.0),
                    close_button_size: Some(28.0),
                    ..Default::default()
                };
            }
            island.configure(&navigation);
            let case = format!(
                "navigation/geometry/{name}/{}",
                if custom { "custom" } else { "preset" }
            );
            measure(&case, 20, || {
                let layout = island.tabs.appearance.layout(1600.0, 1.25, 12, 240.0);
                let mut hits = 0;
                for step in 0..20 {
                    let x = std::hint::black_box(step as f32 * 64.0);
                    let y = std::hint::black_box(island.tabs.appearance.center_y());
                    if let Some(index) = island.tab_at(&layout, x, y) {
                        hits += 1 + island.close_button_hit(&layout, index, x, y) as u64;
                    }
                }
                hits
            });
        }
    }
}
