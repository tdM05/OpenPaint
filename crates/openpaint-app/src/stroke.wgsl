// GPU dab rasterization and stroke compositing.
//
// Implements DECISIONS 4a's per-pixel half: the core decides where dabs go, this
// turns them into pixels. Three passes:
//
//   1. `dab_*`   - stamp dabs into a single-channel accumulation texture.
//   2. `paint_*` - composite the accumulated stroke over a target, either the
//                  canvas texture (baking, at stroke end) or the surface
//                  (previewing, mid-stroke).
//
// # Why accumulation blending is free
//
// The model is `a += flow * coverage * (1 - a)` per dab (see
// openpaint_core::stroke). With blend factors (One, OneMinusSrc) that is exactly
// what the blend unit computes: `dst = src + dst * (1 - src)`. So the fragment
// shader only outputs `flow * coverage` and the hardware does the accumulation --
// no read-modify-write, no ping-pong target.
//
// # The duplicated falloff curve
//
// `dab_fs` below is a *second* implementation of `Dab::coverage_at_distance`, and
// two copies of a curve can drift. That is a real risk, mitigated deliberately:
// `tests/gpu_matches_cpu.rs` rasterizes the same dabs through both paths and
// compares the pixels. If you change the curve here, change it there, and the test
// is what tells you if you forgot.

// ---------------------------------------------------------------- dab stamping

struct CanvasSize {
    // Canvas dimensions in pixels; z/w unused, present for 16-byte alignment.
    size: vec4<f32>,
};

@group(0) @binding(0) var<uniform> canvas: CanvasSize;

struct DabInst {
    @location(0) center: vec2<f32>,
    @location(1) radius: f32,
    @location(2) hardness: f32,
    @location(3) flow: f32,
};

struct DabOut {
    @builtin(position) pos: vec4<f32>,
    // Offset from the dab centre in canvas pixels. Interpolated, so at each
    // fragment this is the pixel *centre* relative to the dab -- matching what the
    // CPU reference samples.
    @location(0) local: vec2<f32>,
    @location(1) radius: f32,
    @location(2) hardness: f32,
    @location(3) flow: f32,
};

@vertex
fn dab_vs(@builtin(vertex_index) vid: u32, inst: DabInst) -> DabOut {
    var offsets = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    // One pixel of slack so the antialiased rim is never clipped by the quad.
    let extent = inst.radius + 1.0;
    let offset = offsets[vid] * extent;
    let p = inst.center + offset;

    var out: DabOut;
    out.pos = vec4<f32>(
        p.x / canvas.size.x * 2.0 - 1.0,
        1.0 - p.y / canvas.size.y * 2.0,
        0.0,
        1.0,
    );
    out.local = offset;
    out.radius = inst.radius;
    out.hardness = inst.hardness;
    out.flow = inst.flow;
    return out;
}

@fragment
fn dab_fs(in: DabOut) -> @location(0) vec4<f32> {
    let dist = length(in.local);
    // Solid core radius; outside it, coverage ramps to zero at the edge.
    let inner = max(in.radius * in.hardness, 0.0);
    var coverage: f32;
    if (dist <= inner) {
        coverage = 1.0;
    } else if (dist >= in.radius) {
        coverage = 0.0;
    } else {
        coverage = 1.0 - (dist - inner) / (in.radius - inner);
    }
    let deposit = coverage * clamp(in.flow, 0.0, 1.0);
    // Same value in every channel: the target is single-channel, and writing it to
    // .a as well keeps the blend valid whichever factor a pipeline picks.
    return vec4<f32>(deposit, deposit, deposit, deposit);
}

// ------------------------------------------------------- stroke compositing

struct Paint {
    // Stroke colour, linear and premultiplied (alpha normally 1).
    color: vec4<f32>,
    // Ceiling for the whole stroke; w/z unused.
    opacity: vec4<f32>,
};

// Canvas corners in NDC, packed two per vec4, matching crate::view::Placement.
struct Placement {
    tl_tr: vec4<f32>,
    bl_br: vec4<f32>,
};

@group(0) @binding(0) var<uniform> paint: Paint;
@group(0) @binding(1) var accum_tex: texture_2d<f32>;
@group(0) @binding(2) var accum_samp: sampler;
@group(1) @binding(0) var<uniform> placement: Placement;

struct PaintOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

fn quad_uv(vid: u32) -> vec2<f32> {
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return uvs[vid];
}

/// Bake: cover the whole target exactly, for compositing into the canvas texture.
@vertex
fn paint_vs_identity(@builtin(vertex_index) vid: u32) -> PaintOut {
    let uv = quad_uv(vid);
    var out: PaintOut;
    out.pos = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

/// Preview: follow the on-screen canvas quad, so the in-progress stroke pans,
/// zooms, and rotates with the canvas.
@vertex
fn paint_vs_placed(@builtin(vertex_index) vid: u32) -> PaintOut {
    let uv = quad_uv(vid);
    let top = mix(placement.tl_tr.xy, placement.tl_tr.zw, uv.x);
    let bottom = mix(placement.bl_br.xy, placement.bl_br.zw, uv.x);

    var out: PaintOut;
    out.pos = vec4<f32>(mix(top, bottom, uv.y), 0.0, 1.0);
    out.uv = uv;
    return out;
}

@fragment
fn paint_fs(in: PaintOut) -> @location(0) vec4<f32> {
    let accumulated = textureSample(accum_tex, accum_samp, in.uv).r;
    // Opacity caps the stroke's total contribution -- this is the whole reason
    // accumulation is kept separate from the canvas until the stroke ends.
    let a = clamp(accumulated, 0.0, 1.0) * paint.opacity.x;
    // Premultiplied output, so the target's (One, OneMinusSrcAlpha) blend is a
    // plain Porter-Duff "over".
    return vec4<f32>(paint.color.rgb * a, a);
}
