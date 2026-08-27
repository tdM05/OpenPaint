// Draws the canvas texture as a single quad, fitted and centered in the window.
// The quad's corners are supplied in normalized device coordinates via a
// uniform, so the CPU controls placement/scale (fit now; pan/zoom later).

struct Placement {
    // Top-left and bottom-right of the quad in NDC (x right, y up).
    min_ndc: vec2<f32>,
    max_ndc: vec2<f32>,
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

    // uv.x 0..1 -> min..max NDC x; uv.y 0..1 -> max..min NDC y (y flips).
    let x = mix(placement.min_ndc.x, placement.max_ndc.x, uv.x);
    let y = mix(placement.min_ndc.y, placement.max_ndc.y, uv.y);

    var out: VsOut;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(canvas_tex, canvas_sampler, in.uv);
}
