// Draws one image as a full-screen triangle pair.
//
// The vertex stage carries no buffers: the quad corners come from the vertex
// index, and placement (zoom, pan, EXIF orientation) is folded into a single
// affine transform passed as uniforms. That keeps a frame down to one draw
// call with nothing to upload but 32 bytes when the framing changes.

struct Placement {
    // Half-size of the image on screen, in clip space.
    half_size: vec2<f32>,
    // Centre of the image relative to the window centre, in clip space.
    centre: vec2<f32>,
    // Row-major 2x2 mapping quad corners to texture coordinates; this is where
    // the EXIF orientation lives, so decoded pixels are never rewritten.
    orientation: mat2x2<f32>,
}

// How the image's colours become the display's.
//
// The stored pixels are decoded to linear light through the source profile's
// tone curves, moved between primaries by a 3x3 matrix, and re-encoded for the
// surface. Doing it here rather than at decode time costs nothing per frame
// and leaves the decoded pixels as the file stored them.
struct Colour {
    // Linear source RGB to linear display RGB. mat3x3 columns are 16-byte
    // aligned in WGSL, which the Rust side pads to match.
    matrix: mat3x3<f32>,
    // 0 when the image and the display agree and the conversion is skipped.
    convert: u32,
    // Whether the surface expects sRGB-encoded values written by this shader.
    encode_srgb: u32,
    // Whether the surface carries extended-range linear light, where a value
    // above 1.0 is brighter than SDR white rather than an overflow to clip.
    extended_range: u32,
}

@group(0) @binding(0) var image_texture: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;
// Tone curves sampled to linear light: one row per channel, so a profile with
// an arbitrary curve costs the same as a simple gamma.
@group(0) @binding(3) var decode_curves: texture_2d<f32>;
@group(0) @binding(4) var curve_sampler: sampler;
@group(0) @binding(5) var<uniform> colour: Colour;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // Two triangles over the corners (-1,-1) (1,-1) (-1,1) (1,1).
    let corner = vec2<f32>(
        f32((index & 1u) * 2u) - 1.0,
        f32((index >> 1u) & 1u) * 2.0 - 1.0,
    );

    var out: VertexOutput;
    out.clip_position = vec4<f32>(corner * placement.half_size + placement.centre, 0.0, 1.0);

    // Corner space is y-up in clip space and y-down in texture space.
    let oriented = placement.orientation * vec2<f32>(corner.x, -corner.y);
    out.uv = oriented * 0.5 + 0.5;
    return out;
}

// Look one channel up in its sampled tone curve.
//
// The three curves are stacked as rows, sampled linearly, so a value between
// two entries interpolates rather than stepping.
fn to_linear(value: f32, channel: i32) -> f32 {
    let row = (f32(channel) + 0.5) / 3.0;
    return textureSample(decode_curves, curve_sampler, vec2<f32>(value, row)).r;
}

// The sRGB transfer function, linear light to stored value.
fn encode_srgb_channel(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    if clamped <= 0.0031308 {
        return clamped * 12.92;
    }
    return 1.055 * pow(clamped, 1.0 / 2.4) - 0.055;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let sampled = textureSample(image_texture, image_sampler, in.uv);

    // Everything below works in linear light, which is what the texture
    // sampler already hands over: an `*Srgb` texture is linearised by the
    // hardware, and a plain one goes through the profile's own curves.
    var rgb = sampled.rgb;
    if colour.convert != 0u {
        // Stored values to linear light, through the image's own curves.
        let linear = vec3<f32>(
            to_linear(rgb.r, 0),
            to_linear(rgb.g, 1),
            to_linear(rgb.b, 2),
        );
        // Between primaries. On a standard-range surface a colour outside the
        // display's gamut lands outside 0..1 and is clipped below, which is
        // the simplest sensible rendering intent for a viewer; on an
        // extended-range surface it survives, because that surface can carry
        // it.
        rgb = colour.matrix * linear;
    }

    // Alpha is composited against the neutral background so a transparent PNG
    // shows the viewer's own backdrop rather than whatever is behind the
    // window. It is stated as linear light, like everything else here, so the
    // same number means the same grey on every surface — the clear colour in
    // `gpu.rs` is the same value for the same reason.
    let background = vec3<f32>(0.09, 0.09, 0.10);

    // An extended-range linear surface wants the light as it is: 1.0 is SDR
    // white and anything above drives the display's headroom. Clamping here
    // would throw away the highlights the surface exists to carry; encoding
    // here would be the encoding the surface does not expect. Only the floor
    // is held, because negative light is not a colour.
    if colour.extended_range != 0u {
        let light = max(rgb, vec3<f32>(0.0));
        return vec4<f32>(mix(background, light, sampled.a), 1.0);
    }

    let clipped = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    if colour.encode_srgb != 0u {
        // The surface takes sRGB-encoded values, so the background is encoded
        // with the image rather than mixed into it as light.
        let composited = mix(background, clipped, sampled.a);
        return vec4<f32>(
            encode_srgb_channel(composited.r),
            encode_srgb_channel(composited.g),
            encode_srgb_channel(composited.b),
            1.0,
        );
    }

    return vec4<f32>(mix(background, clipped, sampled.a), 1.0);
}
