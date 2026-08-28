// Draws the canvas texture as a single quad, positioned by `crate::view::View`.
//
// The quad is supplied as four independent corners in NDC rather than a min/max
// rectangle, because a rotated canvas is not axis-aligned. The vertex shader
// bilinearly interpolates them, which handles pan, zoom, and rotation uniformly
// and keeps all the camera math on the CPU in one place.

struct Placement {
    // Canvas corners in NDC, packed two per vec4 for unambiguous uniform layout:
    // tl_tr = (top-left.xy, top-right.xy), bl_br = (bottom-left.xy, bottom-right.xy).
    // "Top"/"left" refer to canvas space, so under rotation they are no longer
    // visually up or left.
    tl_tr: vec4<f32>,
    bl_br: vec4<f32>,
};

@group(0) @binding(0) var<uniform> placement: Placement;
@group(0) @binding(1) var canvas_tex: texture_2d<f32>;
@group(0) @binding(2) var canvas_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    // Two triangles forming the quad. uv (0,0) = top-left of the texture.
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let uv = uvs[vid];

    // Bilinear interpolation of the four corners. For an axis-aligned quad this
    // reduces to the old min/max mix; for a rotated one it is still exact, because
    // an affine transform maps the unit square to a parallelogram.
    let top = mix(placement.tl_tr.xy, placement.tl_tr.zw, uv.x);
    let bottom = mix(placement.bl_br.xy, placement.bl_br.zw, uv.x);
    let p = mix(top, bottom, uv.y);

    var out: VsOut;
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(canvas_tex, canvas_sampler, in.uv);
}
