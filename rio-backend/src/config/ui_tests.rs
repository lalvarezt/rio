use super::{
    layout::{Margin, Panel},
    navigation::{CloseButton, TabStyle},
    Config,
};

fn config(text: &str) -> Config {
    toml::from_str(text).unwrap()
}

#[test]
fn default_preset_preserves_upstream_panel_settings() {
    let c = config("");
    assert_eq!(c.panel(), Panel::default());
    assert_eq!(c.navigation.tab_geometry(), TabStyle::Pill.geometry());
}

#[test]
fn single_pane_frame_visibility_merges_without_overriding_preset_geometry() {
    for preset in ["pill", "square"] {
        let mut c = config(&format!("[navigation]\npreset='{preset}'\n[panel]\nhide-if-single=true\n[platform.windows.panel]\nhide-if-single=false"));
        assert!(c.panel().hide_if_single);
        let geometry = c.panel();
        c.overwrite_with_platform_config(c.platform.windows.clone().unwrap());
        assert!(!c.panel().hide_if_single);
        assert_eq!(c.panel().margin, geometry.margin);
        assert_eq!(c.panel().border_radius, geometry.border_radius);
    }
}

#[test]
fn square_and_legacy_name_share_defaults() {
    for text in [
        "preset = 'square'",
        "tab-style = 'square'",
        "tab-style = 'windows'",
    ] {
        let c = config(&format!("[navigation]\n{text}"));
        let tabs = c.navigation.tab_geometry();
        assert_eq!(tabs.margin.top, 0.0);
        assert_eq!(tabs.margin.left, 0.0);
        assert_eq!(tabs.gap, 0.0);
        assert_eq!(tabs.radius, 0.0);
        assert_eq!(tabs.close_button, CloseButton::Always);
        assert_eq!(c.panel().border_radius, 0.0);
        assert_eq!(c.panel().margin, Margin::all(0.0));
    }
}

#[test]
fn explicit_overrides_win_independently_of_preset() {
    let c = config(
        "\
[navigation]
preset = 'square'
[navigation.tabs]
height = 72
margin = [10, 16, 8, 12]
padding = [3, 9, 5, 11]
gap = 7
radius = 13
font-size = 21
icon-gap = 5
close-button = 'never'
[panel]
margin = [2, 4]
padding = [10]
row-gap = 5
column-gap = 9
divider-width = 3
border-width = 0
border-radius = 12
",
    );
    let tabs = c.navigation.tab_geometry();
    assert_eq!(tabs.height, 72.0);
    assert_eq!(tabs.content_top(), 64.0);
    assert_eq!(tabs.margin, Margin::new(10.0, 16.0, 8.0, 12.0));
    assert_eq!(tabs.padding, Margin::new(3.0, 9.0, 5.0, 11.0));
    assert_eq!(tabs.gap, 7.0);
    assert_eq!(tabs.radius, 13.0);
    assert_eq!(tabs.font_size, 21.0);
    assert_eq!(tabs.close_button, CloseButton::Never);
    let panel = c.panel();
    assert_eq!(panel.margin, Margin::new(2.0, 4.0, 2.0, 4.0));
    assert_eq!(panel.padding, Margin::all(10.0));
    assert_eq!(panel.row_gap, 5.0);
    assert_eq!(panel.column_gap, 9.0);
    assert_eq!(panel.divider_width, 3.0);
    assert_eq!(panel.border_width, 0.0);
    assert_eq!(panel.border_radius, 12.0);
}

#[test]
fn square_can_be_reproduced_with_explicit_pill_overrides() {
    let square = config("[navigation]\npreset = 'square'");
    let manual = config(
        "\
[navigation]
preset = 'pill'
[navigation.tabs]
margin = [0, 8, 6, 0]
gap = 0
radius = 0
close-button = 'always'
[panel]
margin = [0]
border-width = 1
border-radius = 0
",
    );
    assert_eq!(square.panel(), manual.panel());
    assert_eq!(
        square.navigation.tab_geometry(),
        manual.navigation.tab_geometry()
    );
}

#[test]
fn platform_overrides_merge_fields_before_resolving() {
    let mut c = config(
        "\
[navigation.tabs]
gap = 11
font-size = 20
[panel]
column-gap = 9
[platform.linux.navigation]
preset = 'square'
[platform.linux.navigation.tabs]
gap = 0
[platform.linux.panel]
row-gap = 5
divider-width = 2
",
    );
    c.overwrite_with_platform_config(c.platform.linux.clone().unwrap());
    assert_eq!(c.navigation.tab_geometry().gap, 0.0);
    assert_eq!(c.navigation.tab_geometry().font_size, 20.0);
    assert_eq!(c.panel().column_gap, 9.0);
    assert_eq!(c.panel().row_gap, 5.0);
    assert_eq!(c.panel().divider_width, 2.0);
    assert_eq!(c.panel().border_radius, 0.0);
}

#[test]
fn serialization_keeps_overrides_sparse_and_margins_readable() {
    let c = config("[navigation]\npreset = 'square'\n[navigation.tabs]\ngap = 0\nmargin = [0, 8, 6, 0]\n[panel]\ncolumn-gap = 7");
    let text = toml::to_string(&c).unwrap();
    let mut round_trip = config(&text);
    assert_eq!(round_trip.panel(), c.panel());
    assert_eq!(
        round_trip.navigation.tab_geometry(),
        c.navigation.tab_geometry()
    );
    round_trip.navigation.tab_style = TabStyle::Pill;
    assert_eq!(
        round_trip.panel().border_radius,
        Panel::default().border_radius
    );
    assert_eq!(round_trip.panel().column_gap, 7.0);
    assert_eq!(round_trip.navigation.tab_geometry().gap, 0.0);
    assert_eq!(round_trip.navigation.tab_geometry().radius, 19.0);
}

#[test]
fn invalid_numbers_are_finite_and_large_content_fits() {
    let mut c = config("[navigation]\npreset = 'square'\n[navigation.tabs]\nheight = -1\ngap = nan\nradius = -2\nfont-size = 96\n[panel]\nrow-gap = -5\nborder-radius = inf");
    c.navigation.tabs.padding = Some(Margin::new(4.0, f32::NAN, 7.0, -4.0));
    let tabs = c.navigation.tab_geometry();
    assert!(tabs.height.is_finite());
    assert!(tabs.content_top() - tabs.margin.top >= tabs.font_size + 11.0);
    assert_eq!(tabs.radius, 0.0);
    assert_eq!(tabs.gap, 0.0);
    assert_eq!(tabs.padding.right, 14.0);
    assert_eq!(tabs.padding.left, 0.0);
    assert_eq!(c.panel().row_gap, 0.0);
    assert_eq!(c.panel().border_radius, 0.0);
}

#[test]
fn hidden_close_button_does_not_inflate_height() {
    let c = config("[navigation]\ntab-icon-size=8\n[navigation.tabs]\nheight=32\nfont-size=8\nclose-button='never'\nclose-button-size=96");
    assert_eq!(c.navigation.tab_geometry().height, 32.0);
}

#[test]
fn explicit_preset_files_reproduce_defaults_without_style_branches() {
    for (text, preset, other) in [
        (
            include_str!("../../../misc/presets/pill.toml"),
            TabStyle::Pill,
            TabStyle::Square,
        ),
        (
            include_str!("../../../misc/presets/square.toml"),
            TabStyle::Square,
            TabStyle::Pill,
        ),
    ] {
        let mut explicit = config(text);
        let mut defaults = Config::default();
        defaults.navigation.tab_style = preset;
        explicit.navigation.tab_style = other;
        assert_eq!(
            explicit.navigation.tab_geometry(),
            defaults.navigation.tab_geometry()
        );
        assert_eq!(explicit.panel(), defaults.panel());
    }
}
