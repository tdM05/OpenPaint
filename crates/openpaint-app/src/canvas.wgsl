// Draws the canvas: a paper sheet, then the layer stack composited per tile.
//
// The camera arrives as a 2x3 affine (page -> NDC) rather than four precomputed corners,
// because each tile positions itself from its own coordinate — only the shader knows where a
// given instance goes. `crate::view::PageToNdc` builds the affine by sampling the same forward
// transform input uses, so the two cannot drift.
//
// # Storage is larger than the page, on purpose
//
// Tiles outside the page rectangle are kept, so a crop destroys nothing (DECISIONS 5c). They
// must not be *drawn*, though: the page is the sheet the artist sees. Each tile quad is
// therefore clipped to the page in page space — not with a scissor rectangle, which could not
// work once the canvas is rotated on screen.
//
// # Compositing happens here, in one pass
//
// Every layer of a page lives in the same array texture, so one fragment shader can read them
// all: it walks the stack bottom-first, sampling each layer's tile for this position and
// blending it over what is beneath. That is why blend modes need no destination read and no
// intermediate target (DECISIONS 4e).
//
// The in-progress stroke is injected *into* the active layer as the walk reaches it, rather
// than drawn as a separate pass on top. So a mid-stroke preview is the same arithmetic as the
// committed result, not a lookalike that can disagree with it.

const ABSENT: u32 = 0xffffffffu;

struct Params {
    // page -> NDC: ndc.x = dot(x_row.xyz, vec3(page_pos, 1.0)).
    x_row: vec4<f32>,
    y_row: vec4<f32>,
    // The page rectangle in page coordinates: (x0, y0, x1, y1).
    page: vec4<f32>,
    // Paper colour, linear and premultiplied. The bottom of the stack.
    paper: vec4<f32>,
    // In-progress stroke: rgb is its colour (linear, unpremultiplied), w its opacity ceiling.
    stroke: vec4<f32>,
    // x = layer count, y = index of the active layer, z = 1 while a stroke is in progress.
    counts: vec4<u32>,
    // x = tile size in pixels.
    misc: vec4<f32>,
};

// Per-layer properties, shared by every tile of that layer.
struct LayerInfo {
    blend: u32,
    opacity: f32,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var samp: sampler;
// Which array layer holds each (tile, layer) pair, row-major by instance:
// `slots[instance * layer_count + layer]`, or ABSENT where that layer has no tile there.
@group(0) @binding(3) var<storage, read> slots: array<u32>;
@group(0) @binding(4) var<storage, read> infos: array<LayerInfo>;
@group(0) @binding(5) var accum: texture_2d_array<f32>;
// Which accumulation layer holds the in-progress stroke for each tile, by instance, or ABSENT.
// Per tile because accumulation is tiled for the same reason the canvas is (DECISIONS 4d).
@group(0) @binding(6) var<storage, read> stroke_slots: array<u32>;

// Two triangles over the unit square, (0,0) at the top-left in page space.
fn quad(vid: u32) -> vec2<f32> {
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return uvs[vid];
}

fn to_ndc(p: vec2<f32>) -> vec4<f32> {
    let h = vec3<f32>(p, 1.0);
    return vec4<f32>(dot(params.x_row.xyz, h), dot(params.y_row.xyz, h), 0.0, 1.0);
}

// ------------------------------------------------------------------ paper sheet

// Drawn under the tiles so unpainted area — which allocates no tile at all — still reads as
// paper. Without it a fresh canvas would show the backdrop through every gap.
@vertex
fn sheet_vs(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    return to_ndc(mix(params.page.xy, params.page.zw, quad(vid)));
}

@fragment
fn sheet_fs() -> @location(0) vec4<f32> {
    return params.paper;
}

// ------------------------------------------------------------------------ tiles

struct TileInst {
    @location(0) coord: vec2<i32>,
};

struct TileOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Flat because they are indices, not quantities: interpolating either would read a
    // different tile per fragment.
    @location(1) @interpolate(flat) instance: u32,
};

@vertex
fn composite_vs(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
    inst: TileInst,
) -> TileOut {
    let ts = params.misc.x;
    let t0 = vec2<f32>(inst.coord) * ts;
    let t1 = t0 + vec2<f32>(ts, ts);

    // Clip to the page. `max(..., c0)` on the far corner matters: without it a tile wholly
    // outside the page yields an inverted quad, and with back-face culling off that still
    // rasterizes — drawing retained out-of-page pixels over the sheet.
    let c0 = max(t0, params.page.xy);
    let c1 = max(min(t1, params.page.zw), c0);

    let p = mix(c0, c1, quad(vid));

    var out: TileOut;
    out.pos = to_ndc(p);
    // UVs follow the clipped quad, so an edge tile shows only its in-page part.
    out.uv = (p - t0) / ts;
    out.instance = iid;
    return out;
}

/// One separable blend function, matching `openpaint_core::layer::Blend::apply` exactly.
/// Two copies of this exist; the test that composites through both is what keeps them equal.
fn blend_channel(mode: u32, src: f32, dst: f32) -> f32 {
    switch mode {
        case 1u: { return src * dst; }                 // Multiply
        case 2u: { return src + dst - src * dst; }     // Screen
        default: { return src; }                       // Normal
    }
}

/// Composite premultiplied `src` over premultiplied `dst` using a blend mode.
///
/// The PDF/CSS compositing model: blend functions are defined on *straight* colour, so this
/// un-premultiplies both sides, blends, and recombines. Written in the general form rather
/// than assuming an opaque backdrop, so it stays correct if the base ever becomes transparent
/// (exporting with transparency, or a group that is not sitting on paper).
fn blend_over(src: vec4<f32>, dst: vec4<f32>, mode: u32) -> vec4<f32> {
    let sa = src.a;
    let da = dst.a;
    if (sa <= 0.0) {
        return dst;
    }
    // Straight colour. At zero alpha the channels carry no information, so the value chosen
    // does not matter -- it is multiplied by that same zero alpha below.
    let cs = src.rgb / sa;
    let cb = select(vec3<f32>(0.0), dst.rgb / max(da, 1e-6), da > 0.0);
    let b = vec3<f32>(
        blend_channel(mode, cs.r, cb.r),
        blend_channel(mode, cs.g, cb.g),
        blend_channel(mode, cs.b, cb.b),
    );
    // Co = as*(1-ab)*Cs + as*ab*B + (1-as)*ab*Cb, already premultiplied.
    let co = sa * (1.0 - da) * cs + sa * da * b + (1.0 - sa) * da * cb;
    return vec4<f32>(co, sa + da * (1.0 - sa));
}

@fragment
fn composite_fs(in: TileOut) -> @location(0) vec4<f32> {
    let count = params.counts.x;
    // `active` is a reserved word in WGSL.
    let active_layer = params.counts.y;
    let painting = params.counts.z != 0u;
    let stroke_slot = select(ABSENT, stroke_slots[in.instance], painting);

    var dst = params.paper;
    for (var i = 0u; i < count; i = i + 1u) {
        let info = infos[i];
        let slot = slots[in.instance * count + i];

        var src = vec4<f32>(0.0);
        if (slot != ABSENT) {
            // An explicit level, because an implicit one needs derivatives and this is inside
            // a loop -- and because there is no mip chain to choose from anyway.
            src = textureSampleLevel(tiles, samp, in.uv, i32(slot), 0.0);
        }

        // The in-progress stroke belongs *to* the active layer, so it goes on before the
        // layer's own opacity and blend mode are applied -- exactly as baking will do it.
        if (i == active_layer && stroke_slot != ABSENT) {
            let raw = textureSampleLevel(accum, samp, in.uv, i32(stroke_slot), 0.0).r;
            let a = clamp(raw, 0.0, 1.0) * params.stroke.w;
            let s = vec4<f32>(params.stroke.rgb * a, a);
            src = s + src * (1.0 - a);
        }

        if (info.opacity <= 0.0) {
            continue;
        }
        // Premultiplied, so one multiply scales colour and coverage together.
        dst = blend_over(src * info.opacity, dst, info.blend);
    }
    return dst;
}
