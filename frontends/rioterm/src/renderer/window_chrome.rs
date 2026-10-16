#[cfg(target_os = "windows")]
use rio_backend::sugarloaf::font::FontData;
use rio_backend::sugarloaf::font::FontLibrary;
use rio_backend::sugarloaf::text::DrawOpts;
use rio_backend::sugarloaf::Sugarloaf;
use std::time::Instant;

pub const HEADER_HEIGHT: f32 = 54.0;
const BUTTON_WIDTH: f32 = 32.0;

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

pub fn controls_width(width: f32) -> f32 {
    actions(width).len() as f32 * BUTTON_WIDTH + 8.0
}

pub fn fade(value: &mut f32, target: f32, dt: f32) {
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
        let left = width - list.len() as f32 * BUTTON_WIDTH - 8.0;
        if x < left || x >= left + list.len() as f32 * BUTTON_WIDTH {
            return None;
        }
        list.get(((x - left) / BUTTON_WIDTH) as usize).copied()
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

    pub fn render(&mut self, sugarloaf: &mut Sugarloaf, width: f32, color: [f32; 4]) {
        #[cfg(feature = "profiling")]
        let _timing = crate::profiling::span("header.render");
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
            );
        }
        let list = if self.toolbar_enabled {
            actions(width)
        } else {
            window_actions()
        };
        let left = width - list.len() as f32 * BUTTON_WIDTH - 8.0;
        for (i, action) in list.iter().enumerate() {
            let x = left + i as f32 * BUTTON_WIDTH;
            let index = ACTIONS.iter().position(|a| a == action).unwrap();
            let hover = self.opacity[index];
            let backdrop = if *action == HeaderAction::Close {
                [0.88, 0.24, 0.29, hover * 0.85]
            } else {
                [color[0], color[1], color[2], hover * 0.10]
            };
            sugarloaf.rounded_rect(
                None,
                x + 2.0,
                8.0,
                28.0,
                HEADER_HEIGHT - 16.0,
                backdrop,
                0.05,
                7.0,
                16,
            );
            let glyph = if *action == HeaderAction::Close && hover > 0.5 {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                alpha(color, 0.60 + hover * 0.35)
            };
            let cx = x + BUTTON_WIDTH / 2.0;
            let cy = HEADER_HEIGHT / 2.0;
            #[cfg(target_os = "windows")]
            let icon = match action {
                HeaderAction::NewTab => "\u{e710}",
                HeaderAction::Minimize => "\u{e921}",
                HeaderAction::Maximize if self.maximized => "\u{e923}",
                HeaderAction::Maximize => "\u{e922}",
                HeaderAction::Close => "\u{e8bb}",
            };
            #[cfg(target_os = "windows")]
            if draw_icon(sugarloaf, icon, cx, cy, 12.0, glyph) {
                continue;
            }
            let mut line = |x1, y1, x2, y2| {
                sugarloaf.line(x1, y1, x2, y2, 1.2, 0.0, glyph, 17);
            };
            match action {
                HeaderAction::Minimize => line(cx - 5.0, cy + 2.0, cx + 5.0, cy + 2.0),
                HeaderAction::Close => {
                    line(cx - 4.0, cy - 4.0, cx + 4.0, cy + 4.0);
                    line(cx - 4.0, cy + 4.0, cx + 4.0, cy - 4.0);
                }
                HeaderAction::NewTab => {
                    line(cx - 5.0, cy, cx + 5.0, cy);
                    line(cx, cy - 5.0, cx, cy + 5.0);
                }
                HeaderAction::Maximize => {
                    let inset = if self.maximized { 2.0 } else { 0.0 };
                    line(
                        cx - 5.0,
                        cy - 4.0 + inset,
                        cx + 4.0 - inset,
                        cy - 4.0 + inset,
                    );
                    line(
                        cx + 4.0 - inset,
                        cy - 4.0 + inset,
                        cx + 4.0 - inset,
                        cy + 5.0,
                    );
                    line(cx + 4.0 - inset, cy + 5.0, cx - 5.0, cy + 5.0);
                    line(cx - 5.0, cy + 5.0, cx - 5.0, cy - 4.0 + inset);
                    if self.maximized {
                        line(cx - 2.0, cy - 5.0, cx + 5.0, cy - 5.0);
                        line(cx + 5.0, cy - 5.0, cx + 5.0, cy + 2.0);
                    }
                }
            }
        }
    }
}
