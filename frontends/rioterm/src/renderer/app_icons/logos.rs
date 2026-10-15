use rio_backend::sugarloaf::swash::zeno::{Mask, Transform};
use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, PartialEq, Eq)]
pub struct Logo {
    pub name: &'static str,
    svg: &'static str,
}

pub const LOGOS: &[Logo] = &[
    Logo {
        name: "openai",
        svg: include_str!("logos/openai.svg"),
    },
    Logo {
        name: "claude",
        svg: include_str!("logos/claude.svg"),
    },
];

struct Span {
    x: u16,
    y: u16,
    width: u16,
    alpha: u8,
}
type Cache = FxHashMap<(&'static str, u16), Vec<Span>>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

fn raster(logo: &Logo, size: u16) -> Vec<Span> {
    // Vendored Codicons use a single path and a 16x16 view box. Keep the SVG
    // source intact; rasterize only once per physical size, outside frame work.
    let path = logo
        .svg
        .split_once("<path d=\"")
        .and_then(|(_, tail)| tail.split_once('"'))
        .map(|(path, _)| path)
        .expect("single-path bundled SVG");
    let (mask, _) = Mask::new(path)
        .size(size as u32, size as u32)
        .transform(Some(Transform::scale(
            size as f32 / 16.0,
            size as f32 / 16.0,
        )))
        .render();
    let mut spans: Vec<Span> = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let alpha = mask[y as usize * size as usize + x as usize];
            if alpha == 0 {
                continue;
            }
            if let Some(last) = spans.last_mut() {
                if last.y == y && last.x + last.width == x && last.alpha == alpha {
                    last.width += 1;
                    continue;
                }
            }
            spans.push(Span {
                x,
                y,
                width: 1,
                alpha,
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
        let mut tint = color;
        tint[3] *= span.alpha as f32 / 255.0;
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
