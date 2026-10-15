use resvg::{tiny_skia, usvg};
use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, PartialEq, Eq)]
pub struct Logo {
    pub name: &'static str,
    svg: &'static str,
    tint: bool,
}

macro_rules! logos {
    ($($name:literal: $tint:literal),* $(,)?) => {
        pub const LOGOS: &[Logo] = &[$(Logo {
            name: $name,
            svg: include_str!(concat!("logos/", $name, ".svg")),
            tint: $tint,
        }),*];
    };
}

logos! {
    "openai": true,
    "claude": true,
    "neovim": false,
    "helix": false,
    "superfile": false,
    "television": false,
    "goose": true,
    "jj": false,
    "htop": false,
    "gemini": false,
    "copilot": true,
    "nu": false,
    "opencode": true,
    "zed": true,
    "bash": false,
    "zsh": false,
    "podman": false,
    "julia": false,
    "dotnet": false,
    "bat": false,
    "terraform": false,
    "ansible": true,
    "helm": true,
    "nix": false,
    "postgres": false,
    "redis": false,
    "mysql": false,
    "kubernetes": false,
    "vim": false,
    "git": false,
    "docker": false,
    "python": false,
    "node": false,
    "rust": true,
    "go": false,
    "lua": true,
    "ruby": false,
    "php": false,
}

struct Span {
    x: u16,
    y: u16,
    width: u16,
    color: [u8; 4],
}
type Cache = FxHashMap<(&'static str, u16), Vec<Span>>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn raster(logo: &Logo, size: u16) -> Vec<Span> {
    let tree = usvg::Tree::from_str(logo.svg, &usvg::Options::default())
        .expect("valid bundled SVG");
    let source = tree.size();
    let scale = size as f32 / source.width().max(source.height());
    let transform = tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (size as f32 - source.width() * scale) / 2.0,
        (size as f32 - source.height() * scale) / 2.0,
    );
    let mut pixmap =
        tiny_skia::Pixmap::new(size as u32, size as u32).expect("bounded icon size");
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let mut spans: Vec<Span> = Vec::new();
    for (y, row) in pixmap.pixels().chunks(size as usize).enumerate() {
        for (x, pixel) in row.iter().enumerate() {
            if pixel.alpha() == 0 {
                continue;
            }
            let pixel = pixel.demultiply();
            let color = [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()];
            let (x, y) = (x as u16, y as u16);
            if let Some(last) = spans.last_mut() {
                if last.y == y && last.x + last.width == x && last.color == color {
                    last.width += 1;
                    continue;
                }
            }
            spans.push(Span {
                x,
                y,
                width: 1,
                color,
            });
        }
    }
    spans
}

#[allow(clippy::too_many_arguments)]
pub fn draw(
    sugarloaf: &mut Sugarloaf,
    logo: &'static Logo,
    cx: f32,
    cy: f32,
    size: f32,
    color: [f32; 4],
    order: u8,
    occlusion: Option<[f32; 2]>,
) {
    let scale = sugarloaf.scale_factor();
    let size = (size * scale).round().clamp(1.0, 256.0) as u16;
    let x = (cx * scale - size as f32 / 2.0).round() / scale;
    let y = (cy * scale - size as f32 / 2.0).round() / scale;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(FxHashMap::default()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for span in cache
        .entry((logo.name, size))
        .or_insert_with(|| raster(logo, size))
    {
        let left = x + span.x as f32 / scale;
        let right = left + span.width as f32 / scale;
        let ranges = match occlusion {
            Some([start, end]) => [[left, right.min(start)], [left.max(end), right]],
            None => [[left, right], [right, right]],
        };
        let mut tint = if logo.tint {
            color
        } else {
            [
                span.color[0] as f32 / 255.0,
                span.color[1] as f32 / 255.0,
                span.color[2] as f32 / 255.0,
                color[3],
            ]
        };
        tint[3] *= span.color[3] as f32 / 255.0;
        for [start, end] in ranges {
            if end > start {
                sugarloaf.rect(
                    None,
                    start,
                    y + span.y as f32 / scale,
                    end - start,
                    1.0 / scale,
                    tint,
                    0.0,
                    order,
                );
            }
        }
    }
}
