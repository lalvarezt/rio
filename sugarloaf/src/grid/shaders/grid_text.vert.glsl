#version 450

// Per-instance text (glyph) vertex shader. Each instance is one
// `CellText` quad (4-vertex triangle strip).
//
// Vertex layout matches `CellText` in `sugarloaf/src/grid/cell.rs`
// (32 bytes). The host-side struct also carries `atlas` (offset 28)
// and `page` (offset 30) bytes that the host uses for bucketing
// cells per (kind, page) before issuing draws — but the shader
// doesn't read either, because the bound descriptor set already
// implies which page is sampled and a push constant carries the kind
// (see `grid_text.frag.glsl`).
//
//   loc 0  R32G32_UINT      glyph_pos    (offset 0)
//   loc 1  R32G32_UINT      glyph_size   (offset 8)
//   loc 2  R16G16_SINT      bearings     (offset 16, sign-ext to ivec2)
//   loc 3  R16G16_UINT      grid_pos     (offset 20, zero-ext to uvec2)
//   loc 4  R8G8B8A8_UNORM   color        (offset 24, → vec4 0..1)
//   loc 5  R8_UINT          bools        (offset 29)
//
// Triangle-strip vertex order (4 vertices, as `cmd_draw(4, N, ...)`):
//   vid 0 → (0, 0)  TL
//   vid 1 → (1, 0)  TR
//   vid 2 → (0, 1)  BL
//   vid 3 → (1, 1)  BR

layout(set = 0, binding = 0, std140) uniform Uniforms {
    mat4 projection;
    vec4 grid_padding;
    vec4 cursor_color;
    vec4 cursor_bg_color;
    vec2 cell_size;
    uvec2 grid_size;
    uvec2 cursor_pos;
    uvec2 _pad_cursor;
    float min_contrast;
    uint flags;
    uint padding_extend;
    uint input_colorspace;
    vec4 panel_rect;
    float panel_radius;
    float _pad_panel0;
    float _pad_panel1;
    float _pad_panel2;
} uniforms;

layout(location = 0) in uvec2 in_glyph_pos;
layout(location = 1) in uvec2 in_glyph_size;
layout(location = 2) in ivec2 in_bearings;
layout(location = 3) in uvec2 in_grid_pos;
layout(location = 4) in vec4  in_color;     // unorm8 → vec4 0..1
layout(location = 5) in uint  in_bools;

layout(location = 0) flat out vec4 out_color;
layout(location = 1)      out vec2 out_tex_coord;

layout(push_constant) uniform Placement {
    layout(offset = 4) uint row_offset;
} placement;

const uint BOOL_IS_CURSOR_GLYPH = 2u;

// Colorspace helpers — same as grid_bg.frag.glsl. We need them in the
// vertex stage too so the foreground color goes through the same
// transform as the bg.
vec3 grid_srgb_to_linear(vec3 c) {
    vec3 lo = c / 12.92;
    vec3 hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return mix(lo, hi, greaterThan(c, vec3(0.04045)));
}
vec3 grid_linear_to_srgb(vec3 c) {
    vec3 lo = c * 12.92;
    vec3 hi = pow(c, vec3(1.0 / 2.4)) * 1.055 - 0.055;
    return mix(lo, hi, greaterThan(c, vec3(0.0031308)));
}
// The Vulkan swapchain uses SRGB_NONLINEAR, so convert wide-gamut
// terminal inputs to sRGB rather than the Metal renderer's Display-P3.
vec3 grid_p3_to_srgb(vec3 linear_p3) {
    return vec3(
        dot(linear_p3, vec3( 1.22494018, -0.22494018,  0.0)),
        dot(linear_p3, vec3(-0.04205695,  1.04205695,  0.0)),
        dot(linear_p3, vec3(-0.01963755, -0.07863605,  1.09827360))
    );
}

vec3 grid_rec2020_to_srgb(vec3 linear_r2020) {
    return vec3(
        dot(linear_r2020, vec3( 1.66049100, -0.58764114, -0.07284986)),
        dot(linear_r2020, vec3(-0.12455047,  1.13289990, -0.00834943)),
        dot(linear_r2020, vec3(-0.01815076, -0.10057890,  1.11872966))
    );
}

vec3 grid_prepare_output_rgb(vec3 srgb, uint cs) {
    if (cs == 0u) {
        return srgb;
    }
    vec3 lin = grid_srgb_to_linear(srgb);
    if (cs == 1u) {
        lin = grid_p3_to_srgb(lin);
    } else if (cs == 2u) {
        lin = grid_rec2020_to_srgb(lin);
    }
    return grid_linear_to_srgb(clamp(lin, 0.0, 1.0));
}

void main() {
    // Cell origin in pixel space.
    uvec2 grid_pos = in_grid_pos + uvec2(0, placement.row_offset);
    vec2 cell_pos = uniforms.cell_size * vec2(grid_pos);

    // Quad corner 0..1 from vertex id (4-vertex TRIANGLE_STRIP).
    vec2 corner;
    corner.x = float(gl_VertexIndex == 1 || gl_VertexIndex == 3);
    corner.y = float(gl_VertexIndex == 2 || gl_VertexIndex == 3);

    // Glyph bbox inside the cell. `bearings.y` is from the *bottom* in
    // font convention; flip to top-down by subtracting from cell_size.y.
    vec2 size   = vec2(in_glyph_size);
    vec2 offset = vec2(in_bearings);
    offset.y = uniforms.cell_size.y - offset.y;

    vec2 quad = cell_pos + size * corner + offset;
    quad.x += uniforms.grid_padding.w; // left
    quad.y += uniforms.grid_padding.x; // top

    gl_Position = uniforms.projection * vec4(quad, 0.0, 1.0);

    // Atlas tex coord in PIXEL space — we sample with `texelFetch`
    // (nearest filter, no normalization needed).
    out_tex_coord = vec2(in_glyph_pos) + size * corner;

    // Foreground color through the same colorspace pipeline as the bg
    // pass. `in_color` is already 0..1 from R8G8B8A8_UNORM.
    vec4 color = in_color;
    color.rgb = grid_prepare_output_rgb(color.rgb, uniforms.input_colorspace);
    color.rgb *= color.a;

    // Cursor cell color swap: if this glyph's cell is under the cursor
    // and it's *not* the cursor glyph itself, override with cursor_color.
    // Skip when cursor_color.a == 0 — that's the hollow / unfocused
    // path where the underlying glyph colour should stay intact.
    bool is_cursor_pos =
        (grid_pos.x == uniforms.cursor_pos.x) &&
        (grid_pos.y == uniforms.cursor_pos.y);
    if ((in_bools & BOOL_IS_CURSOR_GLYPH) == 0u
        && is_cursor_pos
        && uniforms.cursor_color.a > 0.0) {
        vec4 c = uniforms.cursor_color;
        c.rgb = grid_prepare_output_rgb(c.rgb, uniforms.input_colorspace);
        c.rgb *= c.a;
        color = c;
    }

    out_color = color;
}
