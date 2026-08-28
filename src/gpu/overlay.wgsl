// Puts the interface on screen, over the image.
//
// egui draws itself into an `Rgba8UnormSrgb` texture, where it is correct: it
// picks its own fragment path from whether the target is an sRGB format, and
// on an sRGB one it converts gamma to light properly. Measured — the same grey
// read back as linear light came out at 0.2159 there and at 0.5024 when egui
// drew straight onto the extended-range surface, which is 2.35 times too
// bright.
//
// So the interface is composited here instead, by the same code that already
// decides what the surface wants: light for an extended-range surface, an sRGB
// encoding for one that does not encode for us, and untouched values for one
// that does. Whatever the display is doing, the interface and the image are
// answered by the same rule.

struct Surface {
    // Whether the shader has to apply the sRGB transfer function itself.
    encode_srgb: u32,
    // Whether the surface carries extended-range linear light.
    extended_range: u32,
    _padding: vec2<u32>,
}

@group(0) @binding(0) var interface_texture: texture_2d<f32>;
@group(0) @binding(1) var interface_sampler: sampler;
@group(0) @binding(2) var<uniform> surface: Surface;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    // The same corner trick the image pass uses: two triangles over the whole
    // target, with no vertex buffer to bind.
    let corner = vec2<f32>(
        f32((index & 1u) * 2u) - 1.0,
        f32((index >> 1u) & 1u) * 2.0 - 1.0,
    );

    var out: VertexOutput;
    out.clip_position = vec4<f32>(corner, 0.0, 1.0);
    // Clip space is y-up, texture space y-down.
    out.uv = vec2<f32>(corner.x, -corner.y) * 0.5 + 0.5;
    return out;
}

fn encode_srgb_channel(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    if clamped <= 0.0031308 {
        return clamped * 12.92;
    }
    return 1.055 * pow(clamped, 1.0 / 2.4) - 0.055;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // The texture is `*Srgb`, so the hardware hands over linear light already,
    // and the alpha egui produced is premultiplied against it.
    let sampled = textureSample(interface_texture, interface_sampler, in.uv);

    if surface.extended_range != 0u {
        // 1.0 is SDR white here. The interface is deliberately not pushed into
        // the display's headroom: a toolbar brighter than white would compete
        // with the photograph, which is the thing worth looking at.
        return sampled;
    }

    if surface.encode_srgb != 0u {
        // Premultiplied light has to be encoded as it stands; dividing out the
        // alpha first would change what the blend below then does with it.
        return vec4<f32>(
            encode_srgb_channel(sampled.r),
            encode_srgb_channel(sampled.g),
            encode_srgb_channel(sampled.b),
            sampled.a,
        );
    }

    return sampled;
}
