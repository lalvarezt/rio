// Shared premultiplied-ARGB pixel helpers for the CPU paths, which all
// write `0xAARRGGBB` into softbuffer's framebuffer. The alpha byte is
// meaningful: Windows DWM reads it for per-pixel window transparency
// (softbuffer's other presenters ignore it), so every store must keep
// the premultiplied invariant, each color channel <= alpha.

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

#[cfg(test)]
mod tests {
    use super::*;

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
