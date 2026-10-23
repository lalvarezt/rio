// Shared premultiplied-ARGB pixel helpers for the CPU paths, which all
// write `0xAARRGGBB` into softbuffer's framebuffer. The alpha byte is
// meaningful: Windows DWM reads it for per-pixel window transparency
// (softbuffer's other presenters ignore it), so every store must keep
// the premultiplied invariant, each color channel <= alpha.

use wide::u32x4;

#[inline(always)]
pub(crate) fn pack_premul(r: u8, g: u8, b: u8, a: u8) -> u32 {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// alpha = 0xff so alpha-respecting compositors treat the pixel as fully
/// opaque (DWM under `DwmEnableBlurBehindWindow`). For non-transparent
/// windows the OS ignores this byte, so writing 0xff is a no-op there.
#[inline(always)]
pub(crate) fn pack_opaque(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Scalar premultiplied Porter-Duff source-over with exact `/255`
/// rounding, all four channels. Preserving the destination alpha through
/// the blend is what lets `window.opacity < 1` survive across
/// overlapping paints instead of getting reset by every draw.
#[inline]
pub(crate) fn blend_premul_over(src: [u8; 4], dst: u32) -> u32 {
    let sa = src[3] as u32;
    if sa == 0 {
        return dst;
    }
    if sa == 255 {
        return pack_opaque(src[0], src[1], src[2]);
    }
    let inv = 255 - sa;
    let source = pack_premul(src[0], src[1], src[2], src[3]);
    // Two independent 16-bit lanes hold R+B or A+G. Each product fits
    // in its lane, including the bias for exact rounded division by 255.
    let pair = |source: u32, destination: u32| {
        let product = destination * inv + 0x0080_0080;
        let scaled = ((product + ((product >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
        let sum = source + scaled;
        // Keep the scalar helper's saturation even for non-premultiplied input.
        let overflow = sum & 0x0100_0100;
        (sum | (overflow - (overflow >> 8))) & 0x00ff_00ff
    };
    pair(source & 0x00ff_00ff, dst & 0x00ff_00ff)
        | (pair((source >> 8) & 0x00ff_00ff, (dst >> 8) & 0x00ff_00ff) << 8)
}

/// Composite a grayscale atlas glyph into the framebuffer. Clip both surfaces
/// once so the inner loop only visits valid source and destination pixels.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blit_mask(
    buf: &mut [u32],
    buf_w: i32,
    buf_h: i32,
    glyph_x: i32,
    glyph_y: i32,
    gw: i32,
    gh: i32,
    atlas: &[u8],
    atlas_side: usize,
    ax: usize,
    ay: usize,
    color: [u8; 4],
) {
    if color[3] == 0 {
        return;
    }
    let x_start = glyph_x.max(0);
    let y_start = glyph_y.max(0);
    let x_end = (glyph_x + gw).min(buf_w);
    let y_end = (glyph_y + gh).min(buf_h);
    if x_end <= x_start || y_end <= y_start {
        return;
    }
    let sx = (x_start - glyph_x) as usize + ax;
    let sy = (y_start - glyph_y) as usize + ay;
    if sx >= atlas_side || sy >= atlas_side {
        return;
    }
    let width = ((x_end - x_start) as usize).min(atlas_side - sx);
    let height = ((y_end - y_start) as usize).min(atlas_side - sy);
    let stride = buf_w as usize;
    let [r, g, b, ca] = color.map(u32::from);
    let pair_mask = u32x4::splat(0x00ff_00ff);
    let bias = u32x4::splat(0x0080_0080);
    let source_rb = u32x4::splat((r << 16) | b);
    let source_ag = u32x4::splat((255 << 16) | g);
    let text_alpha = u32x4::splat(ca);
    let round_pair = |product: u32x4| -> u32x4 {
        let biased = product + bias;
        ((biased + ((biased >> 8) & pair_mask)) >> 8) & pair_mask
    };
    let vector_width = width / 4 * 4;
    for row in 0..height {
        let source = (sy + row) * atlas_side + sx;
        let dest = (y_start as usize + row) * stride + x_start as usize;
        let mask = &atlas[source..source + width];
        let pixels = &mut buf[dest..dest + width];
        for (coverage, dst) in mask[..vector_width]
            .chunks_exact(4)
            .zip(pixels[..vector_width].chunks_exact_mut(4))
        {
            if coverage == [0; 4] {
                continue;
            }
            let coverage = u32x4::new([
                u32::from(coverage[0]),
                u32::from(coverage[1]),
                u32::from(coverage[2]),
                u32::from(coverage[3]),
            ]);
            let biased = coverage * text_alpha + u32x4::splat(128);
            let alpha: u32x4 = (biased + (biased >> 8)) >> 8;
            let inv = u32x4::splat(255) - alpha;
            let dest = u32x4::new([dst[0], dst[1], dst[2], dst[3]]);
            let rb = round_pair(source_rb * alpha) + round_pair((dest & pair_mask) * inv);
            let ag = round_pair(source_ag * alpha)
                + round_pair(((dest >> 8) & pair_mask) * inv);
            // Each premultiplied source channel is <= alpha, so adding the
            // attenuated destination cannot overflow either byte lane.
            let out: u32x4 = rb | (ag << 8);
            dst.copy_from_slice(&out.to_array());
        }
        for (&m, dst) in mask[vector_width..].iter().zip(&mut pixels[vector_width..]) {
            let a = (u32::from(m) * ca + 127) / 255;
            if a == 0 {
                continue;
            }
            let src = [
                ((r * a + 127) / 255) as u8,
                ((g * a + 127) / 255) as u8,
                ((b * a + 127) / 255) as u8,
                a as u8,
            ];
            *dst = blend_premul_over(src, *dst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_blit_matches_all_mask_and_text_alpha_pairs() {
        // A full mask ramp plus a three-pixel tail exercises SIMD and scalar
        // paths with every effective alpha, including transparent pixels.
        let atlas: Vec<_> = (0..259).map(|i| i as u8).collect();
        for alpha in 0..=255u8 {
            for rgb in [[0, 0, 0], [255, 255, 255], [37, 180, 91]] {
                let mut actual: Vec<_> =
                    (0..259u32).map(|i| i.wrapping_mul(0x7123_459b)).collect();
                let expected: Vec<_> = atlas
                    .iter()
                    .zip(&actual)
                    .map(|(&m, &dst)| {
                        let a = (u32::from(m) * u32::from(alpha) + 127) / 255;
                        let channel = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
                        blend_premul_over(
                            [channel(rgb[0]), channel(rgb[1]), channel(rgb[2]), a as u8],
                            dst,
                        )
                    })
                    .collect();
                blit_mask(
                    &mut actual,
                    259,
                    1,
                    0,
                    0,
                    259,
                    1,
                    &atlas,
                    259,
                    0,
                    0,
                    [rgb[0], rgb[1], rgb[2], alpha],
                );
                assert_eq!(actual, expected, "alpha={alpha} rgb={rgb:?}");
            }
        }
    }

    #[test]
    fn blend_matches_scalar_rounding_and_saturation() {
        for alpha in 0..=255u32 {
            for channel in 0..=255u32 {
                let dst = pack_premul(
                    channel as u8,
                    (255 - channel) as u8,
                    channel as u8,
                    channel as u8,
                );
                for value in [0, alpha / 2, alpha, 255] {
                    let src = [value as u8, (value / 2) as u8, value as u8, alpha as u8];
                    let expected = if alpha == 0 {
                        dst
                    } else {
                        let blend = |src: u8, dst: u32| {
                            (src as u32 + (dst * (255 - alpha) + 127) / 255).min(255)
                                as u8
                        };
                        pack_premul(
                            blend(src[0], channel),
                            blend(src[1], 255 - channel),
                            blend(src[2], channel),
                            blend(src[3], channel),
                        )
                    };
                    assert_eq!(
                        blend_premul_over(src, dst),
                        expected,
                        "src={src:?} dst={dst:08x}"
                    );
                }
            }
        }
    }

    /// Opaque destinations stay opaque and transparent sources are
    /// identity, alpha byte included: the invariants `window.opacity`
    /// rides on now that the framebuffer carries real alpha.
    #[test]
    fn blend_premul_over_propagates_alpha() {
        assert_eq!(pack_opaque(1, 2, 3) >> 24, 0xff);
        let dst = 0xff20_4060;
        for sa in 0..=255u8 {
            let src = [sa / 2, sa / 3, sa, sa];
            assert_eq!(blend_premul_over(src, dst) >> 24, 0xff, "sa={sa}");
        }
        let translucent = 0x9912_3456;
        assert_eq!(blend_premul_over([0, 0, 0, 0], translucent), translucent);
        // Porter-Duff source-over on the alpha channel.
        let out = blend_premul_over([0x40, 0x20, 0x10, 0x80], translucent);
        let expected = 0x80u32 + (0x99 * (255 - 0x80) + 127) / 255;
        assert_eq!(out >> 24, expected);
    }
}
