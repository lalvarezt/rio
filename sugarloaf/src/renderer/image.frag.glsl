#version 450

// Image fragment shader, ported from `image_fs_main` in
// `sugarloaf/src/renderer/image.metal`. The texture is
// `R8G8B8A8_SRGB`, so the HW sRGB-decodes bytes on sample —
// bilinear filtering happens in *linear* light, matching Metal's
// `RGBA8Unorm_sRGB` behaviour. We then `linear_to_srgb` back to
// gamma encoding for the framebuffer (plain `B8G8R8A8_UNORM`,
// gamma-space alpha blending — same as the rest of sugarloaf).

layout(set = 0, binding = 0, std140) uniform Globals {
    mat4 transform;
    uint input_colorspace;
    uint _pad0;
    uint _pad1;
    uint _pad2;
} globals;

layout(set = 1, binding = 0) uniform sampler2D image_texture;

layout(location = 0) in vec2 in_tex_coord;

layout(location = 0) out vec4 out_color;

vec3 linear_to_srgb(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = pow(c, vec3(1.0 / 2.4)) * 1.055 - 0.055;
    return mix(lo, hi, greaterThan(c, vec3(0.0031308)));
}

void main() {
    // R8G8B8A8_SRGB: HW decodes the byte → linear before handing it
    // to the shader. Alpha is linear by convention regardless of
    // format. Images use sRGB independently of the terminal's configured
    // input colorspace. Encode back to gamma-sRGB for the sRGB surface.
    vec4 rgba = texture(image_texture, in_tex_coord);
    vec3 enc = linear_to_srgb(rgba.rgb);
    // Premultiply alpha — pipeline blend factors are ONE / ONE_MINUS_SRC_ALPHA.
    out_color = vec4(enc * rgba.a, rgba.a);
}
