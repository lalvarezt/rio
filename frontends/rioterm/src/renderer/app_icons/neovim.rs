// Geometry and colors from Neovim's official flat mark:
// https://github.com/neovim/neovim.github.io/blob/master/static/logos/neovim-mark-flat.svg

use rio_backend::sugarloaf::Sugarloaf;
use rustc_hash::FxHashMap;
use std::sync::{Mutex, OnceLock};

const LEFT: &[(f32, f32)] = &[
    (1.0, 159.437),
    (27.166, 133.0),
    (156.0, 324.998),
    (156.0, 731.0),
    (1.0, 576.202),
];
const RIGHT: &[(f32, f32)] = &[
    (599.94, 160.914),
    (443.0, 3.0),
    (443.0, 407.297),
    (574.295, 605.0),
    (601.0, 575.971),
];
const CROSS: &[(f32, f32)] = &[
    (155.986, 4.0),
    (559.0, 619.19),
    (446.225, 732.0),
    (43.0, 118.172),
];

struct Pixel {
    x: u16,
    y: u16,
    width: u16,
    color: [f32; 4],
}
static CACHE: OnceLock<Mutex<FxHashMap<u16, Vec<Pixel>>>> = OnceLock::new();

fn inside(x: f32, y: f32, points: &[(f32, f32)]) -> bool {
    let mut hit = false;
    let mut previous = points[points.len() - 1];
    for &(px, py) in points {
        if (py > y) != (previous.1 > y)
            && x < (previous.0 - px) * (y - py) / (previous.1 - py) + px
        {
            hit = !hit;
        }
        previous = (px, py);
    }
    hit
}

fn raster(height: u16) -> Vec<Pixel> {
    let width = (height as f32 * 601.0 / 736.0).ceil() as u16;
    let mut pixels: Vec<Pixel> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let mut rgb = [0.0; 3];
            let mut samples = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = (x as f32 + (sx as f32 + 0.5) / 4.0) * 736.0 / height as f32;
                    let fy = (y as f32 + (sy as f32 + 0.5) / 4.0) * 736.0 / height as f32;
                    let color = if inside(fx, fy, CROSS) || inside(fx, fy, RIGHT) {
                        Some([87.0, 161.0, 67.0])
                    } else if inside(fx, fy, LEFT) {
                        Some([60.0, 146.0, 210.0])
                    } else {
                        None
                    };
                    if let Some(color) = color {
                        samples += 1;
                        for i in 0..3 {
                            rgb[i] += color[i];
                        }
                    }
                }
            }
            if samples > 0 {
                let color = [
                    rgb[0] / samples as f32 / 255.0,
                    rgb[1] / samples as f32 / 255.0,
                    rgb[2] / samples as f32 / 255.0,
                    samples as f32 / 16.0,
                ];
                if let Some(previous) = pixels.last_mut() {
                    if previous.y == y
                        && previous.x + previous.width == x
                        && previous.color == color
                    {
                        previous.width += 1;
                        continue;
                    }
                }
                pixels.push(Pixel {
                    x,
                    y,
                    width: 1,
                    color,
                });
            }
        }
    }
    pixels
}

pub fn draw(
    sugarloaf: &mut Sugarloaf,
    cx: f32,
    cy: f32,
    size: f32,
    opacity: f32,
    order: u8,
    occlusion: Option<[f32; 2]>,
) {
    let scale = sugarloaf.scale_factor();
    let height = (size * scale).round().clamp(1.0, 256.0) as u16;
    let width = (height as f32 * 601.0 / 736.0).ceil();
    let x = (cx * scale - width / 2.0).round() / scale;
    let y = (cy * scale - height as f32 / 2.0).round() / scale;
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(FxHashMap::default()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for pixel in cache.entry(height).or_insert_with(|| raster(height)) {
        let mut color = pixel.color;
        color[3] *= opacity;
        let left = x + pixel.x as f32 / scale;
        let right = left + pixel.width as f32 / scale;
        let spans = match occlusion {
            Some([start, end]) => [[left, right.min(start)], [left.max(end), right]],
            None => [[left, right], [right, right]],
        };
        for [start, end] in spans {
            if end > start {
                sugarloaf.rect(
                    None,
                    start,
                    y + pixel.y as f32 / scale,
                    end - start,
                    1.0 / scale,
                    color,
                    0.0,
                    order,
                );
            }
        }
    }
}
