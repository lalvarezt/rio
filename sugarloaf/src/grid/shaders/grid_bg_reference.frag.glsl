#version 450

// Original background implementation, retained as an independent pixel oracle.

// Per-cell background fragment shader. One sample per framebuffer pixel:
// look up the owning grid cell, apply `padding_extend` clamping at the
// edges, paint the cursor color where applicable, otherwise return the
// stored CellBg colour through the sRGB↔DisplayP3 chain.
//
// Ported 1:1 from `grid_bg_fragment` in
// `sugarloaf/src/grid/shaders/grid.metal`. Field order in `Uniforms`
// must match `GridUniforms` in `sugarloaf/src/grid/cell.rs` and the
// Metal `Uniforms` struct in grid.metal — std140 layout naturally
// matches the hand-packed byte layout used over there.

// `Uniforms` mirrors `GridUniforms` (192B = 12 × vec4
// blocks under std140). See cell.rs for offsets / sizes; the fields
// here are listed in the same order.
layout(set = 0, binding = 0, std140) uniform Uniforms {
    mat4 projection;        // offset   0
    vec4 grid_padding;      // offset  64  (top, right, bottom, left)
    vec4 cursor_color;      // offset  80
    vec4 cursor_bg_color;   // offset  96
    vec2 cell_size;         // offset 112
    uvec2 grid_size;        // offset 120
    uvec2 cursor_pos;       // offset 128
    uvec2 _pad_cursor;      // offset 136
    float min_contrast;     // offset 144
    uint flags;             // offset 148
    uint padding_extend;    // offset 152
    uint input_colorspace;  // offset 156
    vec4 panel_rect;        // offset 160
    float panel_radius;    // offset 176
    float _pad_panel0;
    float _pad_panel1;
    float _pad_panel2;
} uniforms;

// `CellBg` is 4 bytes (uchar4 rgba) in cell.rs. We expose the storage
// buffer as `uint cells[]` and unpack each entry via
// `unpackUnorm4x8` — that maps byte[0]→x etc., matching the little-
// endian RGBA layout sugarloaf uses on the CPU side.
layout(set = 0, binding = 1, std430) readonly buffer Cells {
    uint cells[];
};

layout(location = 0) out vec4 out_color;

const uint PAD_EXTEND_LEFT  = 1u << 0;
const uint PAD_EXTEND_RIGHT = 1u << 1;
const uint PAD_EXTEND_UP    = 1u << 2;
const uint PAD_EXTEND_DOWN  = 1u << 3;

// ----- colorspace helpers (mirror `grid.metal` 1:1) -----

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

vec3 grid_prepare_output_rgb(vec3 srgb, uint input_colorspace) {
    if (input_colorspace == 0u) {
        return srgb;
    }
    vec3 lin = grid_srgb_to_linear(srgb);
    if (input_colorspace == 1u) {
        lin = grid_p3_to_srgb(lin);
    } else if (input_colorspace == 2u) {
        lin = grid_rec2020_to_srgb(lin);
    }
    return grid_linear_to_srgb(clamp(lin, 0.0, 1.0));
}

void main() {
    float coverage = 1.0;
    if (all(greaterThan(uniforms.panel_rect.zw, vec2(0.0)))) {
        vec2 half_size = uniforms.panel_rect.zw * 0.5;
        float radius = min(uniforms.panel_radius, min(half_size.x, half_size.y));
        vec2 q = abs(gl_FragCoord.xy - uniforms.panel_rect.xy - half_size) - half_size + radius;
        float distance = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
        coverage = clamp(0.5 - distance, 0.0, 1.0);
        if (coverage == 0.0) { discard; }
    }
    // `gl_FragCoord.xy` is the pixel center in framebuffer pixels.
    // Locate the owning grid cell relative to the grid origin
    // (top-left = grid_padding.w / .x).
    ivec2 orig_grid_pos = ivec2(
        floor((gl_FragCoord.xy - uniforms.grid_padding.wx) / uniforms.cell_size)
    );
    ivec2 grid_pos = orig_grid_pos;

    // Horizontal padding extend / discard.
    if (grid_pos.x < 0) {
        if ((uniforms.padding_extend & PAD_EXTEND_LEFT) != 0u) {
            grid_pos.x = 0;
        } else {
            out_color = vec4(0.0);
            return;
        }
    } else if (grid_pos.x > int(uniforms.grid_size.x) - 1) {
        if ((uniforms.padding_extend & PAD_EXTEND_RIGHT) != 0u) {
            grid_pos.x = int(uniforms.grid_size.x) - 1;
        } else {
            out_color = vec4(0.0);
            return;
        }
    }

    // Vertical padding extend / discard.
    if (grid_pos.y < 0) {
        if ((uniforms.padding_extend & PAD_EXTEND_UP) != 0u) {
            grid_pos.y = 0;
        } else {
            out_color = vec4(0.0);
            return;
        }
    } else if (grid_pos.y > int(uniforms.grid_size.y) - 1) {
        if ((uniforms.padding_extend & PAD_EXTEND_DOWN) != 0u) {
            grid_pos.y = int(uniforms.grid_size.y) - 1;
        } else {
            out_color = vec4(0.0);
            return;
        }
    }

    // Cursor block fill: only when this fragment's *original* grid_pos
    // (pre-clamp) matches the cursor cell. Bypassing the clamp here
    // keeps the cursor from leaking into the margin on edge rows.
    if (uniforms.cursor_bg_color.a > 0.0
        && orig_grid_pos.x == int(uniforms.cursor_pos.x)
        && orig_grid_pos.y == int(uniforms.cursor_pos.y))
    {
        vec4 c = uniforms.cursor_bg_color;
        c.rgb = grid_prepare_output_rgb(c.rgb, uniforms.input_colorspace);
        c.rgb *= c.a;
        out_color = c * coverage;
        return;
    }

    // Load + decode the CellBg.
    uint idx = uint(grid_pos.y) * uniforms.grid_size.x + uint(grid_pos.x);
    vec4 color = unpackUnorm4x8(cells[idx]);
    if (color.a == 0.0) {
        out_color = vec4(0.0);
        return;
    }
    color.rgb = grid_prepare_output_rgb(color.rgb, uniforms.input_colorspace);
    color.rgb *= color.a;

    out_color = color * coverage;
}
