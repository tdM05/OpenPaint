// Draws the canvas: a paper sheet, then one instanced quad per resident tile.
//
// The camera arrives as a 2x3 affine (page -> NDC) rather than four precomputed corners,
// because each tile positions itself from its own coordinate — only the shader knows
// where a given instance goes. `crate::view::PageToNdc` builds the affine by sampling the
// same forward transform input uses, so the two cannot drift.
//
// # Storage is larger than the page, on purpose
//
// Tiles outside the page rectangle are kept, so a crop destroys nothing
// (DECISIONS 5c). They must not be *drawn*, though: the page is the sheet the artist
// sees. Each tile quad is therefore clipped to the page in page space — not with a
// scissor rectangle, which could not work once the canvas is rotated on screen.

struct Xform {
    // page -> NDC: ndc.x = dot(x_row.xyz, vec3(page_pos, 1.0)).
    x_row: vec4<f32>,
    y_row: vec4<f32>,
    // The page rectangle in page coordinates: (x0, y0, x1, y1).
    page: vec4<f32>,
    // Paper colour, linear and premultiplied.
    paper: vec4<f32>,
    // x = tile size in pixels.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> xf: Xform;
@group(0) @binding(1) var tiles: texture_2d_array<f32>;
@group(0) @binding(2) var samp: sampler;

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
    return vec4<f32>(dot(xf.x_row.xyz, h), dot(xf.y_row.xyz, h), 0.0, 1.0);
}

// ------------------------------------------------------------------ paper sheet

// Drawn under the tiles so unpainted area — which allocates no tile at all — still reads
// as paper. Without it a fresh canvas would show the backdrop through every gap.
@vertex
fn sheet_vs(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    return to_ndc(mix(xf.page.xy, xf.page.zw, quad(vid)));
}

@fragment
fn sheet_fs() -> @location(0) vec4<f32> {
    return xf.paper;
}

// ------------------------------------------------------------------------ tiles

struct TileInst {
    @location(0) coord: vec2<i32>,
    @location(1) layer: u32,
};

struct TileOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Flat because it is an index, not a quantity: interpolating it would sample a
    // different layer per fragment.
    @location(1) @interpolate(flat) layer: u32,
};

@vertex
fn tile_vs(@builtin(vertex_index) vid: u32, inst: TileInst) -> TileOut {
    let ts = xf.params.x;
    let t0 = vec2<f32>(inst.coord) * ts;
    let t1 = t0 + vec2<f32>(ts, ts);

    // Clip to the page. `max(..., c0)` on the far corner matters: without it a tile
    // wholly outside the page yields an inverted quad, and with back-face culling off
    // that still rasterizes — drawing retained out-of-page pixels over the sheet.
    let c0 = max(t0, xf.page.xy);
    let c1 = max(min(t1, xf.page.zw), c0);

    let p = mix(c0, c1, quad(vid));

    var out: TileOut;
    out.pos = to_ndc(p);
    // UVs follow the clipped quad, so an edge tile shows only its in-page part.
    out.uv = (p - t0) / ts;
    out.layer = inst.layer;
    return out;
}

@fragment
fn tile_fs(in: TileOut) -> @location(0) vec4<f32> {
    return textureSample(tiles, samp, in.uv, i32(in.layer));
}
