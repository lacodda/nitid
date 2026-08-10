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

@group(0) @binding(0) var image_texture: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
@group(0) @binding(2) var<uniform> placement: Placement;

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let colour = textureSample(image_texture, image_sampler, in.uv);
    // The surface is sRGB-encoded by the swapchain; alpha is composited against
    // the neutral background so a transparent PNG does not show window content.
    let background = vec3<f32>(0.09, 0.09, 0.10);
    return vec4<f32>(mix(background, colour.rgb, colour.a), 1.0);
}
