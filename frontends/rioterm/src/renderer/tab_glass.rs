// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::sugarloaf::Sugarloaf;

const STATUS_OPACITY: f32 = 0.8;
const TRACK_OPACITY: f32 = 0.12;

#[derive(Clone, Copy, Default)]
pub(super) struct Status {
    pub color: [f32; 4],
    pub progress: Option<u8>,
    pub width: f32,
}

/// A reflective rim over an existing surface. Only the border receives color;
/// the fill and the native backdrop inside it remain untouched.
pub(super) fn draw(
    sugarloaf: &mut Sugarloaf,
    [x, y, w, h]: [f32; 4],
    radius: f32,
    intensity: f32,
    status: Option<Status>,
    order: u8,
) {
    // Specialize the ordinary glass path so status compositing and contour
    // measurements cost nothing on tabs without a report.
    if let Some(status) = status {
        draw_rim::<true>(sugarloaf, [x, y, w, h], radius, intensity, status, order);
    } else {
        draw_rim::<false>(
            sugarloaf,
            [x, y, w, h],
            radius,
            intensity,
            Status::default(),
            order,
        );
    }
}

fn draw_rim<const HAS_STATUS: bool>(
    sugarloaf: &mut Sugarloaf,
    [x, y, w, h]: [f32; 4],
    radius: f32,
    intensity: f32,
    status: Status,
    order: u8,
) {
    if (intensity <= 0.0 && !HAS_STATUS) || w <= 0.0 || h <= 0.0 {
        return;
    }
    let scale = sugarloaf.scale_factor();
    let pixel = 1.0 / scale;
    let width = (w * scale).round();
    let height = (h * scale).round();
    let r = (radius * scale).min(width / 2.0).min(height / 2.0);
    let stroke = if HAS_STATUS { status.width } else { 1.0 };
    // Straight corners still need side strips, including thick status rims.
    let columns = r.max(stroke).ceil().min((width / 2.0).floor()) as usize;
    let straight = width - 2.0 * r;
    let vertical = height - 2.0 * r;
    let quarter = std::f32::consts::FRAC_PI_2 * r;
    // Clockwise distance starts where the upper-left arc meets the top edge.
    // With square corners, that tangent is the rectangle's upper-left corner.
    let perimeter = 2.0 * (straight + vertical) + 4.0 * quarter;
    let progress = if HAS_STATUS {
        status.progress.filter(|p| *p < 100)
    } else {
        None
    };
    let progress_end = progress
        .filter(|p| *p > 0)
        .map(|p| perimeter * f32::from(p) / 100.0);
    let base_strength = if progress == Some(0) {
        TRACK_OPACITY
    } else {
        STATUS_OPACITY
    };

    // Scan only the rim, in physical pixels. Subtracting the inner contour's
    // coverage gives an antialiased stroke without a background
    // punch-out. Pixel-aligned rects keep the same coverage on CPU and GPU.
    let coverage = |distance: f32| {
        (0.5 - distance).clamp(0.0, 1.0) - (0.5 - stroke - distance).clamp(0.0, 1.0)
    };
    let mut span = |column: f32,
                    row: f32,
                    length: f32,
                    height: f32,
                    alpha: f32,
                    normal: [f32; 2],
                    travel: f32,
                    direction: f32| {
        if alpha <= 0.0 || length <= 0.0 {
            return;
        }
        let light = -0.6 * normal[0] - 0.8 * normal[1];
        let reflection =
            0.06 + 0.40 * light.max(0.0).powi(2) + 0.16 * light.min(0.0).powi(2);
        let shine = reflection * intensity;
        let mut emit = |column: f32, length: f32, strength: f32| {
            if length <= 0.0 {
                return;
            }
            // Composite the original white reflection over the themed rim in
            // one primitive. Unfinished progress retains a faint border track.
            let color = if HAS_STATUS {
                let tint = status.color[3] * strength * (1.0 - shine);
                let opacity = shine + tint;
                let component = |v: f32| (shine + v * tint) / opacity.max(f32::EPSILON);
                [
                    component(status.color[0]),
                    component(status.color[1]),
                    component(status.color[2]),
                    alpha * opacity,
                ]
            } else {
                [1.0, 1.0, 1.0, alpha * shine]
            };
            sugarloaf.rect(
                x + column * pixel,
                y + row * pixel,
                length * pixel,
                height * pixel,
                color,
                0.05,
                order,
            );
        };
        if let Some(end) = progress_end {
            if length == 1.0 {
                emit(
                    column,
                    length,
                    TRACK_OPACITY
                        + (STATUS_OPACITY - TRACK_OPACITY)
                            * (end - travel + 0.5).clamp(0.0, 1.0),
                );
            } else {
                let cut = ((end - travel) * direction).clamp(0.0, length);
                let (before, after) = if direction > 0.0 {
                    (STATUS_OPACITY, TRACK_OPACITY)
                } else {
                    (TRACK_OPACITY, STATUS_OPACITY)
                };
                emit(column, cut, before);
                emit(column + cut, length - cut, after);
            }
        } else {
            emit(column, length, base_strength);
        }
    };
    let mut row = 0;
    while row < height as usize {
        // Along the straight sides, static rims have identical coverage and
        // lighting. One vertical strip also keeps tall pane frames compact.
        let row_height = if progress_end.is_none()
            && row >= columns
            && row < height as usize - columns
        {
            height as usize - columns - row
        } else {
            1
        };
        let py = row as f32 + 0.5;
        let edge_y = py.min(height - py);
        let normal_y = if py < height / 2.0 { -1.0 } else { 1.0 };
        let left = columns as f32;
        let length = width - left * 2.0;
        let alpha = coverage(-edge_y.min(width / 2.0));
        span(
            left,
            row as f32,
            length,
            row_height as f32,
            alpha,
            [0.0, normal_y],
            if normal_y < 0.0 {
                left - r
            } else {
                perimeter / 2.0 + width - r - left
            },
            if normal_y < 0.0 { 1.0 } else { -1.0 },
        );

        let dy = (r - edge_y).max(0.0);
        let outer_left = r - (r * r - dy * dy).max(0.0).sqrt();
        let inner_r = (r - stroke).max(0.0);
        let inner_left = (r - (inner_r * inner_r - dy * dy).max(0.0).sqrt()).max(stroke);
        let first = (outer_left - 1.0).max(0.0).floor() as usize;
        let end = ((inner_left + 1.0).ceil() as usize).min(columns);
        for column in first..end {
            let dx = r - column as f32 - 0.5;
            let normal_length = dx.max(0.0).hypot(dy);
            let distance = normal_length + dx.max(r - edge_y).min(0.0) - r;
            let alpha = coverage(distance);
            if alpha <= 0.0 {
                continue;
            }
            let (nx, ny) = if normal_length > 0.0 {
                (dx.max(0.0) / normal_length, normal_y * dy / normal_length)
            } else if dx > r - edge_y {
                (1.0, 0.0)
            } else {
                (0.0, normal_y)
            };
            // Only determinate progress needs arc positions. Plain glass and
            // whole status rims keep the original scan without trigonometry.
            let arc = if progress_end.is_some() && r > 0.0 {
                dy.atan2(dx.max(0.0)) * r
            } else {
                0.0
            };
            let side = (py - r).clamp(0.0, vertical);
            let (left_travel, right_travel) = if progress_end.is_some() && nx == 0.0 {
                // The nearest contour point can lie on a horizontal edge,
                // including inside a square corner's inward stroke.
                if normal_y < 0.0 {
                    (-dx, straight + dx)
                } else {
                    (perimeter / 2.0 + straight + dx, perimeter / 2.0 - dx)
                }
            } else {
                (
                    perimeter - quarter - side - normal_y * arc,
                    straight + quarter + side + normal_y * arc,
                )
            };
            span(
                column as f32,
                row as f32,
                1.0,
                row_height as f32,
                alpha,
                [-nx, ny],
                left_travel,
                1.0,
            );
            span(
                width - column as f32 - 1.0,
                row as f32,
                1.0,
                row_height as f32,
                alpha,
                [nx, ny],
                right_travel,
                1.0,
            );
        }
        row += row_height;
    }
}
