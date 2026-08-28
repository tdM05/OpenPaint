// GPU dab rasterization and stroke compositing, tile by tile.
//
// Implements DECISIONS 4a's per-pixel half: the core decides where dabs go, this turns
// them into pixels. Three passes:
//
//   1. `dab_*`     - stamp dabs into one tile of the accumulation pool.
//   2. `bake_*`    - composite an accumulation tile into the matching canvas tile, once,
//                    when the stroke ends.
//
// There is deliberately no preview pass here. Mid-stroke, the *compositor* reads this
// accumulation and injects it into the active layer as it walks the stack (DECISIONS 4e), so
// the preview and the committed result are the same arithmetic rather than two lookalikes.
//
// # Why accumulation blending is free
//
// The model is `a += flow * coverage * (1 - a)` per dab (see openpaint_core::stroke).
// With blend factors (One, OneMinusSrc) that is exactly what the blend unit computes:
// `dst = src + dst * (1 - src)`. So the fragment shader only outputs `flow * coverage`
// and the hardware does the accumulation -- no read-modify-write, no ping-pong target.
//
// # Why accumulation is tiled too
//
// Flow accumulates per dab while opacity caps the stroke total, so a stroke has to stay
// separable from what is underneath it right up until it commits. That means the
// accumulation buffer must cover everywhere the stroke has been -- which is potentially
// the whole canvas. A page-sized accumulation texture would reintroduce exactly the
// ceiling the tiled canvas removed, so accumulation uses its own sparse tile pool, one
// layer per tile the stroke has touched.
//
// # Painting is clipped to the page; storage is not
//
// Dab quads are clamped to the page rectangle in *page* space. Clamping the position and
// deriving the distance-from-centre from the clamped point keeps coverage exact, because
// the falloff is a function of that distance and nothing else. It also means the bake
// needs no clipping of its own: paint outside the page was never accumulated.
//
// # The duplicated falloff curve
//
// `dab_fs` below is a *second* implementation of `Dab::coverage_at_distance`, and two
// copies of a curve drift. That is a real risk, mitigated deliberately: the tests in
// `stroke_layer.rs` rasterize the same dabs through both paths and compare pixels. If you
// change the curve here, change it there, and the test is what tells you if you forgot.

// Stroke colour and its ceiling.
struct Paint {
    // Linear and premultiplied (alpha normally 1).
    color: vec4<f32>,
    // x = ceiling for the whole stroke; rest unused.
    opacity: vec4<f32>,
};

// Per-tile parameters, selected with a dynamic offset.
//
// Dynamic offsets rather than one write per tile: `Queue::write_buffer` applies every
// write in a submission *before* any of its command buffers run, so rewriting one uniform
// between passes would leave every pass reading the last tile's values. That hazard has
// already produced one visible bug in this app (see `upload_dabs`).
// The page rectangle deliberately lives only in `Xform`, so clipping has one source of
// truth shared by every pass.
struct TileParams {
    // xy = tile origin in page coordinates, z = tile size in pixels.
    tile: vec4<f32>,
    // x = accumulation layer for this tile.
    layer: vec4<u32>,
};

// page -> NDC, matching crate::view::PageToNdc.
struct Xform {
    x_row: vec4<f32>,
    y_row: vec4<f32>,
    page: vec4<f32>,
    params: vec4<f32>,  // x = tile size
};

@group(0) @binding(0) var<uniform> paint: Paint;
@group(0) @binding(1) var accum: texture_2d_array<f32>;
@group(0) @binding(2) var accum_samp: sampler;
@group(1) @binding(0) var<uniform> tp: TileParams;
@group(2) @binding(0) var<uniform> xf: Xform;

// Two triangles over the unit square, (0,0) at the top-left in page space.
fn quad(vid: u32) -> vec2<f32> {
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return uvs[vid];
}

// ---------------------------------------------------------------- dab stamping

struct DabInst {
    @location(0) center: vec2<f32>,
    @location(1) radius: f32,
    @location(2) hardness: f32,
    @location(3) flow: f32,
};

struct DabOut {
    @builtin(position) pos: vec4<f32>,
    // Offset from the dab centre in page pixels. Interpolated, so at each fragment this is
    // the pixel *centre* relative to the dab -- matching what the CPU reference samples.
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
    let raw = inst.center + offsets[vid] * extent;

    // Clip to the page. Per-axis clamping keeps the quad an axis-aligned rectangle, and
    // the two triangles clamp their shared corners identically, so the quad stays
    // watertight. A dab entirely outside the page collapses to zero area and draws
    // nothing.
    let p = clamp(raw, xf.page.xy, xf.page.zw);

    // Page coordinates -> this tile's own texture space. Geometry outside the tile is
    // clipped by the render target, so no per-tile culling is needed on the CPU.
    let ts = tp.tile.z;
    let local_px = p - tp.tile.xy;

    var out: DabOut;
    out.pos = vec4<f32>(
        local_px.x / ts * 2.0 - 1.0,
        1.0 - local_px.y / ts * 2.0,
        0.0,
        1.0,
    );
    // Derived from the *clamped* position, so coverage is still exactly a function of
    // distance from the dab centre.
    out.local = p - inst.center;
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
    // Same value in every channel: the target is single-channel, and writing it to .a as
    // well keeps the blend valid whichever factor a pipeline picks.
    return vec4<f32>(deposit, deposit, deposit, deposit);
}

// ------------------------------------------------------- stroke compositing

struct PaintOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Flat because it is an index, not a quantity.
    @location(1) @interpolate(flat) layer: u32,
};

/// Bake: cover one canvas tile exactly. The accumulation tile and the canvas tile are the
/// same size and aligned, so the quad is the whole target and uv is the quad.
@vertex
fn bake_vs(@builtin(vertex_index) vid: u32) -> PaintOut {
    let q = quad(vid);
    var out: PaintOut;
    out.pos = vec4<f32>(q.x * 2.0 - 1.0, 1.0 - q.y * 2.0, 0.0, 1.0);
    out.uv = q;
    out.layer = tp.layer.x;
    return out;
}

@fragment
fn paint_fs(in: PaintOut) -> @location(0) vec4<f32> {
    let accumulated = textureSample(accum, accum_samp, in.uv, i32(in.layer)).r;
    // Opacity caps the stroke's total contribution -- the whole reason accumulation is
    // kept separate from the canvas until the stroke ends.
    let a = clamp(accumulated, 0.0, 1.0) * paint.opacity.x;
    // Premultiplied output, so the target's (One, OneMinusSrcAlpha) blend is a plain
    // Porter-Duff "over".
    return vec4<f32>(paint.color.rgb * a, a);
}
