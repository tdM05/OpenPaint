// Drawing the floating selection: one canvas tile, resampled from the lifted pixels.
//
// The CPU walked every destination pixel and ran a twenty-tap filter over the source for each one
// -- 33 ms for a 256-pixel selection and 453 ms for a 1024-pixel one, per pointer sample. This is
// the same walk with the same arithmetic, done by the machine that walks pixels for a living.
//
// # The inverse, not the forward transform
//
// Reconstruction goes *backwards*: each destination pixel asks where it came from. Walking source
// pixels forward leaves holes wherever the transform magnifies. The arithmetic below is
// `Transform::invert` written out in WGSL -- deliberately the same expression, in the same order,
// so the preview and the commit cannot disagree about where a pixel goes.
//
// # The border is why there is no bounds check
//
// The source texture is uploaded with one transparent pixel of margin on every side, so
// `ClampToEdge` extends transparency rather than smearing the outermost row of artwork across the
// page. A test that rotates a solid square and looks at the corners is what that margin is for.

struct Params {
    /// Top-left of the destination tile, in page pixels.
    dest: vec4<f32>,
    /// Top-left of the source texture in page pixels, and its size in pixels.
    src: vec4<f32>,
    /// The pivot the transform turns about, and the translation after it.
    pivot_offset: vec4<f32>,
    /// The effective scale, and the sine and cosine of the rotation.
    scale_rot: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_samp: sampler;

/// A full-target triangle pair, as (0,0)..(1,1).
///
/// Six vertices rather than four and an index buffer: the whole geometry of this pass is one
/// rectangle, and a buffer to describe it would be more machinery than the thing it describes.
fn quad(vid: u32) -> vec2<f32> {
    var c = vec2<f32>(0.0, 0.0);
    switch vid {
        case 0u: { c = vec2<f32>(0.0, 0.0); }
        case 1u: { c = vec2<f32>(1.0, 0.0); }
        case 2u: { c = vec2<f32>(0.0, 1.0); }
        case 3u: { c = vec2<f32>(0.0, 1.0); }
        case 4u: { c = vec2<f32>(1.0, 0.0); }
        default: { c = vec2<f32>(1.0, 1.0); }
    }
    return c;
}

struct Out {
    @builtin(position) pos: vec4<f32>,
    /// Where this fragment is on the page, in page pixels.
    @location(0) page: vec2<f32>,
};

@vertex
fn float_vs(@builtin(vertex_index) vid: u32) -> Out {
    let q = quad(vid);
    var out: Out;
    out.pos = vec4<f32>(q.x * 2.0 - 1.0, 1.0 - q.y * 2.0, 0.0, 1.0);
    // `dest.z` carries the tile's size in pixels, so the shader needs no constant of its own.
    out.page = p.dest.xy + q * p.dest.z;
    return out;
}

@fragment
fn float_fs(in: Out) -> @location(0) vec4<f32> {
    // The centre of this destination pixel, which is what the filter is asking about.
    let here = in.page + vec2<f32>(0.5, 0.5);

    // `Transform::invert`, exactly: undo the translation, then the rotation, then the scale.
    let pivot = p.pivot_offset.xy;
    let offset = p.pivot_offset.zw;
    let scale = p.scale_rot.xy;
    let sinr = p.scale_rot.z;
    let cosr = p.scale_rot.w;
    let d = here - pivot - offset;
    let r = vec2<f32>(d.x * cosr + d.y * sinr, -d.x * sinr + d.y * cosr);
    // `source`, not `from`: WGSL reserves that word.
    let source = pivot + r / scale;

    // Into the source texture. The margin means a sample just outside the artwork reads the
    // transparent border rather than the edge of the picture.
    let uv = (source - p.src.xy) / p.src.zw;
    return textureSample(src, src_samp, uv);
}
