use rio_backend::config::colors::Colors;
use rio_backend::config::navigation::Animation;
use rio_backend::sugarloaf::font::FontData;
use rio_backend::sugarloaf::font::FontLibrary;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use std::time::Instant;

pub const HEADER_HEIGHT: f32 = 54.0;
const BUTTON_WIDTH: f32 = 36.0;
const CONTROL_GAP: f32 = 10.0;
const CONTROL_MARGIN: f32 = 8.0;
const CONTROL_INSET: f32 = 2.0;
const CONTROL_ORDER: u8 = 16;
const GLYPH_HALF_SIZE: f32 = 5.0;
const GLYPH_STROKE: f32 = 1.2;
const RESTORE_OFFSET: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    NewTab,
    Minimize,
    Maximize,
    Close,
}

const ACTIONS: [HeaderAction; 4] = [
    HeaderAction::NewTab,
    HeaderAction::Minimize,
    HeaderAction::Maximize,
    HeaderAction::Close,
];

fn actions(width: f32) -> &'static [HeaderAction] {
    if width >= 480.0 {
        #[cfg(target_os = "windows")]
        return &ACTIONS;
        #[cfg(not(target_os = "windows"))]
        return &ACTIONS[..1];
    }
    window_actions()
}

fn window_actions() -> &'static [HeaderAction] {
    #[cfg(target_os = "windows")]
    {
        &ACTIONS[1..]
    }
    #[cfg(not(target_os = "windows"))]
    &[]
}

fn group_width(list: &[HeaderAction]) -> f32 {
    list.len() as f32 * BUTTON_WIDTH
        + if list.len() > 1 && list[0] == HeaderAction::NewTab {
            CONTROL_GAP
        } else {
            0.0
        }
}

fn button_x(list: &[HeaderAction], index: usize, width: f32) -> f32 {
    width - CONTROL_MARGIN - group_width(list)
        + index as f32 * BUTTON_WIDTH
        + if index > 0 && list[0] == HeaderAction::NewTab {
            CONTROL_GAP
        } else {
            0.0
        }
}

pub fn controls_width(width: f32) -> f32 {
    group_width(actions(width)) + CONTROL_MARGIN
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
    pub danger: [f32; 4],
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
            danger: colors.red,
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
    opacity: [f32; 4],
    last_frame: Instant,
    font_library: Option<FontLibrary>,
    pub app_font: Option<usize>,
    pub theme: Theme,
    pub maximized: bool,
    pub fullscreen: bool,
    pub toolbar_enabled: bool,
}

impl Header {
    pub fn new() -> Self {
        Self {
            hover: None,
            opacity: [0.0; 4],
            last_frame: Instant::now(),
            font_library: None,
            app_font: None,
            theme: Theme::new(&Colors::default()),
            maximized: false,
            fullscreen: false,
            toolbar_enabled: true,
        }
    }

    pub fn hit_test(&self, x: f32, y: f32, width: f32) -> Option<HeaderAction> {
        if self.fullscreen || !(6.0..HEADER_HEIGHT - 6.0).contains(&y) {
            return None;
        }
        let list = if self.toolbar_enabled {
            actions(width)
        } else {
            window_actions()
        };
        list.iter().enumerate().find_map(|(index, action)| {
            let left = button_x(list, index, width);
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

    pub fn needs_redraw(&self) -> bool {
        ACTIONS.iter().enumerate().any(|(i, action)| {
            self.opacity[i] != u8::from(self.hover == Some(*action)) as f32
        })
    }

    pub fn render(
        &mut self,
        sugarloaf: &mut Sugarloaf,
        width: f32,
        animation: Animation,
    ) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("header.render");
        let color = self.theme.foreground;
        let library = sugarloaf.font_library();
        if self.font_library.as_ref().is_none_or(|previous| {
            !std::sync::Arc::ptr_eq(&previous.inner, &library.inner)
        }) {
            #[cfg(target_os = "windows")]
            load_font(library, "SegoeMDL2Assets", "segmdl2.ttf");
            self.app_font = super::app_icons::load_font(library);
            self.font_library = Some(library.clone());
        }
        if self.fullscreen {
            self.hover = None;
            self.opacity.fill(0.0);
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        for (i, action) in ACTIONS.iter().enumerate() {
            fade(
                &mut self.opacity[i],
                u8::from(self.hover == Some(*action)) as f32,
                dt,
                animation,
            );
        }
        let list = if self.toolbar_enabled {
            actions(width)
        } else {
            window_actions()
        };
        let base = alpha(color, 0.055);
        for (i, action) in list.iter().enumerate() {
            let x = button_x(list, i, width);
            let index = ACTIONS.iter().position(|a| a == action).unwrap();
            let hover = self.opacity[index];
            // New-tab has its own pill; Windows caption actions share one.
            if *action == HeaderAction::NewTab
                || i == 0
                || list[i - 1] == HeaderAction::NewTab
            {
                let group = if *action == HeaderAction::NewTab {
                    BUTTON_WIDTH
                } else {
                    (list.len() - i) as f32 * BUTTON_WIDTH
                };
                sugarloaf.rounded_rect(
                    None,
                    x,
                    (HEADER_HEIGHT - BUTTON_WIDTH) / 2.0,
                    group,
                    BUTTON_WIDTH,
                    base,
                    0.05,
                    BUTTON_WIDTH / 2.0,
                    CONTROL_ORDER,
                );
            }
            if hover > 0.0 {
                let tint = if *action == HeaderAction::Close {
                    self.theme.danger
                } else {
                    color
                };
                sugarloaf.rounded_rect(
                    None,
                    x + CONTROL_INSET,
                    (HEADER_HEIGHT - BUTTON_WIDTH) / 2.0 + CONTROL_INSET,
                    BUTTON_WIDTH - CONTROL_INSET * 2.0,
                    BUTTON_WIDTH - CONTROL_INSET * 2.0,
                    alpha(
                        tint,
                        hover
                            * if *action == HeaderAction::Close {
                                0.22
                            } else {
                                0.10
                            },
                    ),
                    0.05,
                    BUTTON_WIDTH / 2.0 - CONTROL_INSET * 2.0,
                    CONTROL_ORDER + 1,
                );
            }
            // An opaque secondary tint keeps intersecting strokes uniform.
            let glyph = mix(self.theme.surface, color, 0.72 + hover * 0.28);
            let glyph = if *action == HeaderAction::Close {
                mix(glyph, self.theme.danger, hover)
            } else {
                glyph
            };
            let cx = x + BUTTON_WIDTH / 2.0;
            let cy = HEADER_HEIGHT / 2.0;
            // Caption geometry avoids font discovery, shaping, and platform
            // glyph bearings on every frame.
            let scale = sugarloaf.scale_factor();
            let stroke = (GLYPH_STROKE * scale).round().max(1.0) / scale;
            let align = |v| {
                snap(v, scale)
                    + if (stroke * scale) as u32 % 2 == 1 {
                        0.5 / scale
                    } else {
                        0.0
                    }
            };
            let mut line = |x1, y1, x2, y2| {
                sugarloaf.line(
                    align(x1),
                    align(y1),
                    align(x2),
                    align(y2),
                    stroke,
                    0.0,
                    glyph,
                    CONTROL_ORDER + 2,
                );
            };
            let half = GLYPH_HALF_SIZE;
            match action {
                HeaderAction::Minimize => line(cx - half, cy, cx + half, cy),
                HeaderAction::Close => {
                    line(cx - half, cy - half, cx + half, cy + half);
                    line(cx - half, cy + half, cx + half, cy - half);
                }
                HeaderAction::NewTab => {
                    line(cx - half, cy, cx + half, cy);
                    line(cx, cy - half, cx, cy + half);
                }
                HeaderAction::Maximize => {
                    let edge = half - if self.maximized { RESTORE_OFFSET } else { 0.0 };
                    let right = cx + edge;
                    let top = cy - edge;
                    line(cx - half, top, right, top);
                    line(right, top, right, cy + half);
                    line(right, cy + half, cx - half, cy + half);
                    line(cx - half, cy + half, cx - half, top);
                    if self.maximized {
                        line(cx - edge, cy - half, cx + half, cy - half);
                        line(cx + half, cy - half, cx + half, cy + edge);
                    }
                }
            }
        }
    }
}
