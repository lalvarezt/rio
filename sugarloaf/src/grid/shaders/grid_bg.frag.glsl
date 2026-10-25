#version 450

// Per-cell background coverage and exact pixel ownership. Colors arrive flat
// from the vertex shader; padding and cursor ownership match the reference.
//
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

layout(location = 0) flat in vec4 in_color;
layout(location = 1) flat in uvec2 in_grid_pos;
layout(location = 2) flat in vec4 in_cursor_color;
layout(location = 0) out vec4 out_color;

const uint PAD_EXTEND_LEFT = 1u;
const uint PAD_EXTEND_RIGHT = 2u;
const uint PAD_EXTEND_UP = 4u;
const uint PAD_EXTEND_DOWN = 8u;

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

    // Conservative quads can overlap. Only the owning cell may blend this pixel.
    if (any(notEqual(grid_pos, ivec2(in_grid_pos)))) { discard; }

    // Cursor block fill: only when this fragment's *original* grid_pos
    // (pre-clamp) matches the cursor cell. Bypassing the clamp here
    // keeps the cursor from leaking into the margin on edge rows.
    if (uniforms.cursor_bg_color.a > 0.0
        && orig_grid_pos.x == int(uniforms.cursor_pos.x)
        && orig_grid_pos.y == int(uniforms.cursor_pos.y))
    {
        out_color = in_cursor_color * coverage;
        return;
    }

    out_color = in_color * coverage;
}
