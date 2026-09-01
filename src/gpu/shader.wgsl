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
    // The part of this draw's texture that carries the pixels being shown.
    // An image small enough for one texture uses the whole of it — scale 1,
    // offset 0 — and a tile of a larger one uses the sub-rectangle inside its
    // padding. See `tiles.rs` and ADR 0015.
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
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
    // What shows through a transparent pixel: 0 the viewer's own dark scene,
    // 1 a checkerboard, 2 black, 3 white. See `Backdrop` in `gpu.rs`.
    backdrop: u32,
    // Whether to mark the pixels the file itself clipped.
    zebra: u32,
    // Whether the texture hands this shader linear light rather than the
    // file's stored values. True for the `*Srgb` texture an untouched 8-bit
    // image is uploaded into, where the hardware linearises on sampling — the
    // zebra has to undo that to see what the file actually stored.
    sampled_is_linear: u32,
    // Padding to a 16-byte boundary, since WGSL aligns the struct's size and
    // Rust's `repr(C)` does not. Matched by `_padding` in `ColourUniform`.
    _padding: vec2<u32>,
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
    // The inset is applied after the orientation because it names a rectangle
    // of the texture as stored, and `oriented` is already in that space: the
    // orientation maps a screen corner to the texel it should show, and the
    // inset then picks that texel out of this tile rather than out of the
    // whole image.
    out.uv = (oriented * 0.5 + 0.5) * placement.uv_scale + placement.uv_offset;
    return out;
}

// How wide one square of the checkerboard is, in physical pixels.
//
// Measured on screen rather than in the image, so the pattern stays the same
// size at any zoom: a checker that scaled with the picture would read as part
// of the picture, which is the one thing it must not do.
const CHECKER_SIZE: f32 = 12.0;

// What shows through a transparent pixel, as linear light.
//
// The scene stays dark by decision, so that is the default; the other three
// exist because judging a cut-out against one backdrop is judging it against
// one background, and a logo bound for a white page has to be seen on white.
fn backdrop_at(position: vec2<f32>) -> vec3<f32> {
    switch colour.backdrop {
        // A checkerboard, in the two greys the convention uses, kept dark
        // enough not to glare beside the viewer's own scene.
        case 1u: {
            let square = floor(position / CHECKER_SIZE);
            let dark = (square.x + square.y) % 2.0 < 1.0;
            // sRGB 0.20 and 0.28 as linear light.
            return select(vec3<f32>(0.0331), vec3<f32>(0.0648), dark);
        }
        case 2u: {
            return vec3<f32>(0.0);
        }
        case 3u: {
            return vec3<f32>(1.0);
        }
        default: {
            return vec3<f32>(0.09, 0.09, 0.10);
        }
    }
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

// How wide one stripe of the zebra is, in physical pixels.
//
// Measured on screen like the checkerboard, and for the same reason: a hatch
// that scaled with the picture would read as part of it.
const ZEBRA_SIZE: f32 = 7.0;

// The sRGB transfer function the other way: linear light back to the stored
// value.
//
// Needed only by the zebra, and only for an image uploaded into an `*Srgb`
// texture, where the hardware linearised on sampling and the value the file
// actually holds is no longer in hand.
fn decode_srgb_channel(value: f32) -> f32 {
    if value <= 0.04045 / 12.92 {
        return value * 12.92;
    }
    return 1.055 * pow(value, 1.0 / 2.4) - 0.055;
}

// Whether this pixel is one the *file* clipped, and which way.
//
// Judged on the file's own stored values, before any conversion — the same
// decision the histogram is built on (ADR 0019). A highlight this display
// cannot reproduce is not a highlight the camera blew, and marking it would
// tell the photographer to fix something that is not wrong with the picture.
//
// Returns 1 for a blown highlight, -1 for a blocked shadow, 0 otherwise.
fn clipping_of(stored: vec3<f32>) -> f32 {
    // A whisker below the ends rather than exactly at them: an 8-bit 255
    // arrives as 1.0, but a 16-bit sample a step below full scale is 0.99998,
    // and a JPEG's 254 is a highlight already gone for practical purposes.
    let high = any(stored >= vec3<f32>(0.996));
    let low = all(stored <= vec3<f32>(0.004));
    if high {
        return 1.0;
    }
    if low {
        return -1.0;
    }
    return 0.0;
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

    // The zebra, over the converted colour but before any encoding: it is
    // stated as light, like everything else here, so it means the same thing
    // on every surface.
    //
    // Marked on the file's own stored values rather than on what came out of
    // the transform, which is the decision ADR 0019 records for the
    // histogram: a highlight this display cannot reach is not one the camera
    // blew. On an `*Srgb` texture the hardware already linearised, so the
    // stored value is reconstructed to ask the question of the right numbers.
    if colour.zebra != 0u && sampled.a > 0.0 {
        var stored = sampled.rgb;
        if colour.sampled_is_linear != 0u {
            stored = vec3<f32>(
                decode_srgb_channel(stored.r),
                decode_srgb_channel(stored.g),
                decode_srgb_channel(stored.b),
            );
        }

        let clipping = clipping_of(stored);
        if clipping != 0.0 {
            // Diagonal stripes, so the hatch cannot be mistaken for anything
            // in the picture: nothing photographic is a 45-degree comb.
            let diagonal = in.clip_position.x + in.clip_position.y;
            if (diagonal - ZEBRA_SIZE * floor(diagonal / ZEBRA_SIZE)) < ZEBRA_SIZE * 0.5 {
                // Blown highlights are marked in red and blocked shadows in
                // blue: the two failures are opposite and must not look alike.
                // Stated as linear light at a strength that reads over both a
                // white highlight and a black shadow.
                // Written as a branch rather than `select`: measured, the
                // two colours came out swapped — a blown highlight marked
                // blue and a blocked shadow red — because the argument order
                // is easy to state backwards and nothing but a rendered
                // frame shows it. A branch says which is which.
                if clipping > 0.0 {
                    rgb = vec3<f32>(0.7, 0.0, 0.0);
                } else {
                    rgb = vec3<f32>(0.0, 0.05, 0.6);
                }
            }
        }
    }

    // Alpha is composited against the chosen backdrop so a transparent PNG
    // shows something of the viewer's rather than whatever is behind the
    // window. It is stated as linear light, like everything else here, so the
    // same number means the same grey on every surface — the clear colour in
    // `gpu.rs` is the same value for the same reason.
    let background = backdrop_at(in.clip_position.xy);

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
