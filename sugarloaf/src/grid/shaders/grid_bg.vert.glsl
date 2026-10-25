#version 450

// One instance per cell. Resolve its color before rasterization instead of
// loading and converting it independently for every covered pixel.
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

layout(set = 0, binding = 1, std430) readonly buffer Cells {
    uint cells[];
};
layout(location = 0) flat out vec4 out_color;
layout(location = 1) flat out uvec2 out_grid_pos;
layout(location = 2) flat out vec4 out_cursor_color;

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

vec3 grid_srgb_to_p3(vec3 linear_srgb) {
    return vec3(
        dot(linear_srgb, vec3(0.82246197, 0.17753803, 0.0)),
        dot(linear_srgb, vec3(0.03319420, 0.96680580, 0.0)),
        dot(linear_srgb, vec3(0.01708263, 0.07239744, 0.91051993))
    );
}

vec3 grid_rec2020_to_p3(vec3 linear_r2020) {
    return vec3(
        dot(linear_r2020, vec3( 1.34357825, -0.28217967, -0.06139858)),
        dot(linear_r2020, vec3(-0.06529745,  1.08782226, -0.02252481)),
        dot(linear_r2020, vec3( 0.00282179, -0.02598807,  1.02316628))
    );
}

vec3 grid_prepare_output_rgb(vec3 srgb, uint input_colorspace) {
    vec3 lin = grid_srgb_to_linear(srgb);
    if (input_colorspace == 0u) {
        lin = grid_srgb_to_p3(lin);
    } else if (input_colorspace == 2u) {
        lin = grid_rec2020_to_p3(lin);
    }
    return grid_linear_to_srgb(lin);
}


void main() {
    uint index = uint(gl_InstanceIndex);
    uvec2 cell = uvec2(index % uniforms.grid_size.x, index / uniforms.grid_size.x);
    out_grid_pos = cell;
    out_color = vec4(0.0);
    out_cursor_color = vec4(0.0);
    vec4 color = unpackUnorm4x8(cells[index]);
    bool cursor = all(equal(cell, uniforms.cursor_pos))
        && uniforms.cursor_bg_color.a > 0.0;
    if (color.a == 0.0 && !cursor) {
        // Degenerate primitives produce no fragments for transparent cells.
        gl_Position = vec4(0.0, 0.0, 0.0, 1.0);
        return;
    }
    if (color.a > 0.0) {
        color.rgb = grid_prepare_output_rgb(color.rgb, uniforms.input_colorspace);
        color.rgb *= color.a;
        out_color = color;
    }
    if (cursor) {
        vec4 c = uniforms.cursor_bg_color;
        c.rgb = grid_prepare_output_rgb(c.rgb, uniforms.input_colorspace);
        c.rgb *= c.a;
        out_cursor_color = c;
    }

    vec2 corner = vec2(gl_VertexIndex & 1, (gl_VertexIndex >> 1) & 1);
    // Cover conservative pixel bounds. The fragment's original floor/clamp
    // calculation resolves ownership exactly, including fractional cell sizes.
    // Vulkan guarantees at least four bits of subpixel precision. One such
    // step covers rasterization rounding without adding a full pixel of overlap.
    const float guard = 1.0 / 16.0;
    vec2 lo = uniforms.grid_padding.wx + vec2(cell) * uniforms.cell_size - guard;
    vec2 hi = uniforms.grid_padding.wx + (vec2(cell) + 1.0) * uniforms.cell_size + guard;
    vec2 pixel = mix(lo, hi, corner);
    gl_Position = uniforms.projection * vec4(pixel, 0.0, 1.0);
    if (cell.x == 0u && corner.x == 0.0 && (uniforms.padding_extend & 1u) != 0u)
        gl_Position.x = -gl_Position.w;
    if (cell.x + 1u == uniforms.grid_size.x && corner.x == 1.0 && (uniforms.padding_extend & 2u) != 0u)
        gl_Position.x = gl_Position.w;
    // Production flips both the projection and Vulkan viewport vertically.
    // Padding follows framebuffer edges under either coordinate convention.
    float top_clip = uniforms.projection[1][1] < 0.0 ? 1.0 : -1.0;
    if (cell.y == 0u && corner.y == 0.0 && (uniforms.padding_extend & 4u) != 0u)
        gl_Position.y = top_clip * gl_Position.w;
    if (cell.y + 1u == uniforms.grid_size.y && corner.y == 1.0 && (uniforms.padding_extend & 8u) != 0u)
        gl_Position.y = -top_clip * gl_Position.w;
}
