// Copyright (c) 2023-present, Raphael Amorim.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use rio_backend::sugarloaf::Sugarloaf;

/// A reflective rim over the existing pill. Only the border receives color;
/// the fill and the native backdrop inside it remain untouched.
pub(super) fn draw(
    sugarloaf: &mut Sugarloaf,
    [x, y, w, h]: [f32; 4],
    radius: f32,
    intensity: f32,
    order: u8,
) {
    if intensity <= 0.0 || w <= 0.0 || h <= 0.0 {
        return;
    }
    let scale = sugarloaf.scale_factor();
    let pixel = 1.0 / scale;
    let width = (w * scale).round();
    let height = (h * scale).round();
    let r = (radius * scale).min(width / 2.0).min(height / 2.0);
    let columns = r.ceil().min((width / 2.0).floor()) as usize;

    // Scan only the rim, in physical pixels. Subtracting the inner contour's
    // coverage gives an antialiased, one-pixel stroke without a background
    // punch-out. Pixel-aligned rects keep the same coverage on CPU and GPU.
    let coverage = |distance: f32| {
        (0.5 - distance).clamp(0.0, 1.0) - (-0.5 - distance).clamp(0.0, 1.0)
    };
    let mut span = |column: f32, row: f32, length: f32, alpha: f32, normal: [f32; 2]| {
        if alpha <= 0.0 || length <= 0.0 {
            return;
        }
        let light = -0.6 * normal[0] - 0.8 * normal[1];
        let reflection =
            0.06 + 0.40 * light.max(0.0).powi(2) + 0.16 * light.min(0.0).powi(2);
        sugarloaf.rect(
            None,
            x + column * pixel,
            y + row * pixel,
            length * pixel,
            pixel,
            [1.0, 1.0, 1.0, alpha * reflection * intensity],
            0.05,
            order,
        );
    };
    for row in 0..height as usize {
        let py = row as f32 + 0.5;
        let edge_y = py.min(height - py);
        let normal_y = if py < height / 2.0 { -1.0 } else { 1.0 };
        span(
            columns as f32,
            row as f32,
            width - columns as f32 * 2.0,
            coverage(-edge_y),
            [0.0, normal_y],
        );

        let dy = (r - edge_y).max(0.0);
        let outer_left = r - (r * r - dy * dy).max(0.0).sqrt();
        let inner_r = (r - 1.0).max(0.0);
        let inner_left = r - (inner_r * inner_r - dy * dy).max(0.0).sqrt();
        let first = (outer_left - 1.0).max(0.0).floor() as usize;
        let end = ((inner_left + 1.0).ceil() as usize).min(columns);
        for column in first..end {
            let dx = r - column as f32 - 0.5;
            let normal_length = dx.max(0.0).hypot(dy);
            let distance = normal_length + dx.max(r - edge_y).min(0.0) - r;
            let alpha = coverage(distance);
            let nx = dx.max(0.0) / normal_length.max(f32::EPSILON);
            let ny = normal_y * dy / normal_length.max(f32::EPSILON);
            span(column as f32, row as f32, 1.0, alpha, [-nx, ny]);
            span(
                width - column as f32 - 1.0,
                row as f32,
                1.0,
                alpha,
                [nx, ny],
            );
        }
    }
}
