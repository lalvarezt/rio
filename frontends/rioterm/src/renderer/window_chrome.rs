use rio_backend::config::colors::Colors;
use rio_backend::config::navigation::Animation;
use rio_backend::sugarloaf::font::FontData;
use rio_backend::sugarloaf::font::FontLibrary;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;

pub const HEADER_HEIGHT: f32 = 54.0;
const BUTTON_WIDTH: f32 = 46.0;
const BUTTON_HEIGHT: f32 = 32.0;
const CONTROL_ORDER: u8 = 16;
const GLYPH_SIZE: f32 = 10.0;
// Windows caption resources, independent of the terminal's ANSI palette.
const CLOSE_HOVER: [f32; 4] = [196.0 / 255.0, 43.0 / 255.0, 28.0 / 255.0, 1.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    Minimize,
    Maximize,
    Close,
}

#[cfg(target_os = "windows")]
const ACTIONS: &[HeaderAction] = &[
    HeaderAction::Minimize,
    HeaderAction::Maximize,
    HeaderAction::Close,
];
#[cfg(not(target_os = "windows"))]
const ACTIONS: &[HeaderAction] = &[];

pub fn controls_width() -> f32 {
    ACTIONS.len() as f32 * BUTTON_WIDTH
}

/// Shared colors for application chrome and overlays.
#[derive(Clone, Copy)]
pub struct Theme {
    pub surface: [f32; 4],
    pub foreground: [f32; 4],
    pub muted: [f32; 4],
    pub border: [f32; 4],
    pub hover: [f32; 4],
    pub accent: [f32; 4],
    pub on_accent: [f32; 4],
}

fn mix(a: [f32; 4], b: [f32; 4], amount: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * amount,
        a[1] + (b[1] - a[1]) * amount,
        a[2] + (b[2] - a[2]) * amount,
        1.0,
    ]
}

impl Theme {
    pub fn new(colors: &Colors) -> Self {
        let foreground = colors.foreground;
        let surface = mix(colors.background.0, foreground, 0.045);
        let accent = colors.cyan;
        let light = accent[0] * 0.2126 + accent[1] * 0.7152 + accent[2] * 0.0722;
        Self {
            surface,
            foreground,
            muted: mix(surface, foreground, 0.58),
            border: mix(surface, foreground, 0.20),
            hover: mix(surface, foreground, 0.09),
            accent,
            on_accent: if light > 0.45 {
                [0.04, 0.04, 0.04, 1.0]
            } else {
                [1.0; 4]
            },
        }
    }
}

/// Resolve a proportional UI face once per font library, outside the draw loop.
pub fn load_ui_font(library: &FontLibrary) -> Option<usize> {
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("chrome.load_ui_font");
    use rio_backend::sugarloaf::font::loader::{Database, Family, Query, Source};
    use rio_backend::sugarloaf::font::Slot;
    let preferred = if cfg!(target_os = "windows") {
        "Segoe UI"
    } else if cfg!(target_os = "macos") {
        "Helvetica Neue"
    } else {
        "Noto Sans"
    };
    let database = Database::new();
    let id = database.query(&Query {
        families: &[Family::Name(preferred), Family::SansSerif],
        ..Query::default()
    })?;
    let (source, index) = database.face_source(id)?;
    let font = match source {
        Source::File(path) => {
            #[cfg(target_os = "macos")]
            {
                FontData::from_path_macos(path, Slot::Regular, &Default::default())
                    .ok()?
            }
            #[cfg(not(target_os = "macos"))]
            {
                FontData::from_discovered_path(path, index).ok()?
            }
        }
        Source::Binary(data) => FontData::from_data(
            data,
            std::path::PathBuf::new(),
            false,
            Slot::Regular,
            &Default::default(),
            index,
        )
        .ok()?,
    };
    if let Some(id) = font
        .postscript_name()
        .and_then(|name| library.font_id_for_postscript_name(name))
    {
        return Some(id);
    }
    let mut fonts = library.inner.write();
    let id = fonts.len();
    fonts.insert(font);
    Some(id)
}

pub fn fade(value: &mut f32, target: f32, dt: f32, animation: Animation) {
    if *value == target {
        return;
    }
    if animation != Animation::Full {
        *value = target;
        return;
    }
    *value += (target - *value) * (1.0 - (-dt / 0.045).exp());
    if (*value - target).abs() < 0.005 {
        *value = target;
    }
}

pub fn alpha(mut color: [f32; 4], opacity: f32) -> [f32; 4] {
    color[3] *= opacity;
    color
}

pub fn snap(value: f32, scale: f32) -> f32 {
    (value * scale).round() / scale
}

pub fn centered_text_y(
    sugarloaf: &mut Sugarloaf,
    text: &str,
    cy: f32,
    opts: &DrawOpts,
) -> f32 {
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("header.vertical_bounds");
    let (top, height) = sugarloaf
        .text_mut()
        .vertical_bounds(text, opts)
        .unwrap_or((0.0, opts.font_size));
    snap(cy - top - height / 2.0, sugarloaf.scale_factor())
}

pub fn fit_text<'a>(
    sugarloaf: &mut Sugarloaf,
    text: &'a str,
    max_width: f32,
    opts: &DrawOpts,
) -> std::borrow::Cow<'a, str> {
    use std::borrow::Cow;
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("chrome.fit_text");
    let ui = sugarloaf.text_mut();
    if ui.measure(text, opts) <= max_width {
        return Cow::Borrowed(text);
    }
    const ELLIPSIS: &str = "…";
    if ui.measure(ELLIPSIS, opts) > max_width {
        return Cow::Borrowed("");
    }
    let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let mut low = 0;
    let mut high = boundaries.len();
    while low < high {
        let mid = (low + high) / 2;
        let candidate = format!("{}{ELLIPSIS}", &text[..boundaries[mid]]);
        if ui.measure(&candidate, opts) <= max_width {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let end = low.checked_sub(1).map_or(0, |i| boundaries[i]);
    Cow::Owned(format!("{}{ELLIPSIS}", &text[..end]))
}

#[cfg(target_os = "windows")]
fn load_font(library: &FontLibrary, name: &str, file: &str) -> Option<usize> {
    #[cfg(feature = "profiling")]
    let _timing = crate::profiling::span("header.load_font");
    if let Some(id) = library.font_id_for_postscript_name(name) {
        return Some(id);
    }
    let directory = std::env::var_os("WINDIR")?;
    let path = std::path::PathBuf::from(directory).join("Fonts").join(file);
    let font = FontData::from_discovered_path(path, 0).ok()?;
    let mut fonts = library.inner.write();
    let id = fonts.len();
    fonts.insert(font);
    Some(id)
}

#[cfg(target_os = "windows")]
pub fn draw_icon(
    sugarloaf: &mut Sugarloaf,
    icon: &str,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
) -> bool {
    let Some(font_id) = sugarloaf
        .font_library()
        .font_id_for_postscript_name("SegoeMDL2Assets")
    else {
        return false;
    };
    draw_font_icon(sugarloaf, icon, cx, cy, size, color, font_id)
}

pub fn draw_font_icon(
    sugarloaf: &mut Sugarloaf,
    icon: &str,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
    font_id: usize,
) -> bool {
    let scale = sugarloaf.scale_factor();
    let opts = DrawOpts {
        font_size: snap(size, scale),
        color: color.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
        font_id: Some(font_id),
        ..DrawOpts::default()
    };
    let y = centered_text_y(sugarloaf, icon, cy, &opts);
    let ui = sugarloaf.text_mut();
    let width = ui.measure(icon, &opts);
    ui.draw(snap(cx - width / 2.0, scale), y, icon, &opts);
    true
}

pub struct Header {
    hover: Option<HeaderAction>,
    font_library: Option<FontLibrary>,
    caption_font: Option<usize>,
    pub app_font: Option<usize>,
    pub theme: Theme,
    pub maximized: bool,
    pub fullscreen: bool,
    pub focused: bool,
}

impl Header {
    pub fn new() -> Self {
        Self {
            hover: None,
            font_library: None,
            caption_font: None,
            app_font: None,
            theme: Theme::new(&Colors::default()),
            maximized: false,
            fullscreen: false,
            focused: true,
        }
    }

    pub fn hit_test(&self, x: f32, y: f32, width: f32) -> Option<HeaderAction> {
        if self.fullscreen || !(0.0..BUTTON_HEIGHT).contains(&y) {
            return None;
        }
        ACTIONS.iter().enumerate().find_map(|(index, action)| {
            let left = width - controls_width() + index as f32 * BUTTON_WIDTH;
            (x >= left && x < left + BUTTON_WIDTH).then_some(*action)
        })
    }

    pub fn hover(&mut self, x: f32, y: f32, width: f32) -> bool {
        self.set_hover(self.hit_test(x, y, width))
    }

    pub fn clear_hover(&mut self) -> bool {
        self.set_hover(None)
    }

    fn set_hover(&mut self, hover: Option<HeaderAction>) -> bool {
        let changed = self.hover != hover;
        self.hover = hover;
        changed
    }

    pub fn render(&mut self, sugarloaf: &mut Sugarloaf, width: f32) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("header.render");
        let library = sugarloaf.font_library();
        if self.font_library.as_ref().is_none_or(|previous| {
            !std::sync::Arc::ptr_eq(&previous.inner, &library.inner)
        }) {
            #[cfg(target_os = "windows")]
            {
                self.caption_font = load_font(library, "SegoeMDL2Assets", "segmdl2.ttf");
            }
            self.app_font = super::app_icons::load_font(library);
            self.font_library = Some(library.clone());
        }
        if self.fullscreen {
            self.hover = None;
            return;
        }
        // Match Windows' light/dark caption resources rather than ANSI colors.
        let dark = self.theme.surface[0] * 0.2126
            + self.theme.surface[1] * 0.7152
            + self.theme.surface[2] * 0.0722
            < 0.5;
        let foreground = if dark { [1.0; 4] } else { [0.0, 0.0, 0.0, 1.0] };
        let hover_fill = alpha(foreground, if dark { 15.0 / 255.0 } else { 9.0 / 255.0 });
        let inactive = alpha(foreground, if dark { 93.0 / 255.0 } else { 92.0 / 255.0 });
        for (index, action) in ACTIONS.iter().enumerate() {
            let x = width - controls_width() + index as f32 * BUTTON_WIDTH;
            let hovered = self.hover == Some(*action);
            let close_hover = hovered && *action == HeaderAction::Close;
            if hovered {
                sugarloaf.rect(
                    None,
                    x,
                    0.0,
                    BUTTON_WIDTH,
                    BUTTON_HEIGHT,
                    if close_hover { CLOSE_HOVER } else { hover_fill },
                    0.0,
                    CONTROL_ORDER,
                );
            }
            let glyph = match action {
                HeaderAction::Minimize => "\u{e921}",
                HeaderAction::Maximize if self.maximized => "\u{e923}",
                HeaderAction::Maximize => "\u{e922}",
                HeaderAction::Close => "\u{e8bb}",
            };
            if let Some(font) = self.caption_font {
                draw_font_icon(
                    sugarloaf,
                    glyph,
                    x + BUTTON_WIDTH / 2.0,
                    BUTTON_HEIGHT / 2.0,
                    GLYPH_SIZE,
                    if close_hover {
                        [1.0; 4]
                    } else if self.focused || hovered {
                        foreground
                    } else {
                        inactive
                    },
                    font,
                );
            }
        }
    }
}
