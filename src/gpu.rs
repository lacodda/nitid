//! The GPU side: device, swapchain, and the single pass that draws the image.
//!
//! nitid configures its own surface rather than delegating to a GUI framework,
//! because HDR output on Windows is only reachable through `Bt2100Pq` on
//! `Rgb10a2Unorm` — a configuration a framework-managed surface cannot express.
//! See `docs/adr/0001-own-the-swapchain.md`. HDR itself lands in v0.6.0; what
//! matters here is that the choice of format stays ours.

use std::sync::Arc;

use anyhow::{Context, Result};
use wgpu::util::DeviceExt;

use crate::color::{CURVE_SAMPLES, ColorTransform};
use crate::image_source::{DecodedImage, Orientation};
use crate::view::View;

/// The colour behind the image. It matches the shader's compositing background
/// so a transparent PNG blends into the window rather than onto a seam.
const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.09,
    g: 0.09,
    b: 0.10,
    a: 1.0,
};

/// Placement uniforms, laid out to match `Placement` in `shader.wgsl`.
///
/// WGSL aligns a `mat2x2<f32>` to 8 bytes and each of its columns to 8, so the
/// two `vec2` fields ahead of it need no padding. The struct is 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Placement {
    half_size: [f32; 2],
    centre: [f32; 2],
    orientation: [[f32; 2]; 2],
}

impl Placement {
    /// Fold zoom, pan, and EXIF orientation into what the vertex stage needs.
    fn new(view: &View, window: (u32, u32), orientation: Orientation) -> Self {
        let window_width = window.0.max(1) as f32;
        let window_height = window.1.max(1) as f32;
        let (scaled_width, scaled_height) = view.scaled_size();
        let (offset_x, offset_y) = view.offset();

        Self {
            // Clip space spans 2 units across the window, hence the halving.
            half_size: [scaled_width / window_width, scaled_height / window_height],
            // Clip space is y-up; a positive pixel offset moves down the screen.
            centre: [2.0 * offset_x / window_width, -2.0 * offset_y / window_height],
            orientation: orientation_matrix(orientation),
        }
    }

    fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

/// The inverse of the EXIF transform, mapping quad corners to texture space.
///
/// Applying orientation here rather than by shuffling decoded bytes keeps a
/// rotated 60-megapixel photo free of a second full-size copy.
fn orientation_matrix(orientation: Orientation) -> [[f32; 2]; 2] {
    match orientation {
        Orientation::Normal => [[1.0, 0.0], [0.0, 1.0]],
        Orientation::FlipHorizontal => [[-1.0, 0.0], [0.0, 1.0]],
        Orientation::Rotate180 => [[-1.0, 0.0], [0.0, -1.0]],
        Orientation::FlipVertical => [[1.0, 0.0], [0.0, -1.0]],
        Orientation::Transpose => [[0.0, 1.0], [1.0, 0.0]],
        Orientation::Rotate90 => [[0.0, 1.0], [-1.0, 0.0]],
        Orientation::Transverse => [[0.0, -1.0], [-1.0, 0.0]],
        Orientation::Rotate270 => [[0.0, -1.0], [1.0, 0.0]],
    }
}

/// Colour conversion state, laid out to match `Colour` in `shader.wgsl`.
///
/// WGSL aligns each column of a `mat3x3<f32>` to 16 bytes, so the matrix is
/// stored as three padded rows rather than nine tight floats.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct ColourUniform {
    matrix: [[f32; 4]; 3],
    /// Non-zero when a conversion is needed. Named to match the shader, where
    /// `active` turned out to be a reserved WGSL keyword.
    convert: u32,
    encode_srgb: u32,
    _padding: [u32; 2],
}

impl ColourUniform {
    fn new(transform: &ColorTransform, surface_is_srgb: bool) -> Self {
        let mut matrix = [[0.0; 4]; 3];
        for (row, values) in transform.matrix.iter().enumerate() {
            matrix[row][..3].copy_from_slice(values);
        }

        Self {
            matrix,
            convert: u32::from(!transform.is_identity),
            // An sRGB surface encodes for us on write. Anything else — the
            // 10-bit HDR surface of v0.6.0, for instance — expects the shader
            // to have done it, but only when the shader produced linear light
            // in the first place.
            encode_srgb: u32::from(!transform.is_identity && !surface_is_srgb),
            _padding: [0; 2],
        }
    }
}

/// The image currently resident on the GPU.
struct Upload {
    bind_group: wgpu::BindGroup,
}

/// Device, surface, pipeline, and the texture being shown.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Tone curves sampled to linear light: three rows, one per channel.
    curves: wgpu::Texture,
    curves_view: wgpu::TextureView,
    curve_sampler: wgpu::Sampler,
    colour: wgpu::Buffer,
    upload: Option<Upload>,
}

impl Renderer {
    /// Bring up a device on `window` and configure the swapchain.
    pub fn new(window: Arc<winit::window::Window>, size: (u32, u32)) -> Result<Self> {
        // Enumerating every backend costs over a hundred milliseconds of
        // startup, because each one loads its driver before being rejected.
        // On Windows only DX12 matters: it is the backend HDR output requires
        // (ADR 0001), so the others are work that can never pay off.
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = if cfg!(windows) { wgpu::Backends::DX12 } else { wgpu::Backends::PRIMARY };
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance.create_surface(window).context("creating a drawing surface for the window")?;

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .context("no graphics adapter can draw to this window")?;

        crate::startup::milestone("adapter chosen");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nitid device"),
            // Downlevel limits keep integrated and older GPUs in scope; nothing
            // in the image path needs more than a texture and one draw call.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("requesting a graphics device")?;

        crate::startup::milestone("device created");

        let capabilities = surface.get_capabilities(&adapter);
        let config = configure(&capabilities, size);
        surface.configure(&device, &config);

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nitid image bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nitid image shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu/shader.wgsl").into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nitid image pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        crate::startup::milestone("shader compiled");

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("nitid image pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nitid image sampler"),
            // Linear filtering both ways: a downscaled photo without it crawls
            // with aliasing, and an upscaled one shows filtering rather than
            // the blocky nearest-neighbour some viewers ship by default.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nitid placement"),
            contents: Placement {
                half_size: [1.0, 1.0],
                centre: [0.0, 0.0],
                orientation: orientation_matrix(Orientation::Normal),
            }
            .as_bytes(),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // One row per channel. `R16Float` rather than `R32Float` because only
        // 16-bit floats are guaranteed filterable — a 32-bit curve texture is
        // rejected on hardware that cannot interpolate it, and interpolation
        // between entries is the whole point. Half precision holds a 0..1
        // curve to about eleven bits, well past what an 8-bit image needs.
        let curves = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nitid tone curves"),
            size: wgpu::Extent3d {
                width: CURVE_SAMPLES as u32,
                height: 3,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let curves_view = curves.create_view(&wgpu::TextureViewDescriptor::default());

        let curve_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nitid curve sampler"),
            // Linear so values between curve entries interpolate. The shader
            // samples each row at its centre, so no filtering happens across
            // channels even though the mode allows it.
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let colour = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nitid colour"),
            contents: bytemuck::bytes_of(&ColourUniform::new(&ColorTransform::identity(), config.format.is_srgb())),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            layout,
            sampler,
            uniforms,
            curves,
            curves_view,
            curve_sampler,
            colour,
            upload: None,
        })
    }

    /// The drawing surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure the swapchain after the window changed size.
    pub fn resize(&mut self, size: (u32, u32)) {
        if size.0 == 0 || size.1 == 0 || self.size() == size {
            return;
        }
        self.config.width = size.0;
        self.config.height = size.1;
        self.surface.configure(&self.device, &self.config);
    }

    /// Upload a decoded image, replacing whatever was shown before.
    ///
    /// `transform` says how the image's colours reach the display. When it is
    /// the identity the texture is created as sRGB and the hardware does the
    /// decoding for free; when a real conversion is needed the raw values are
    /// uploaded instead, because the shader has to apply the image's own tone
    /// curves rather than assume sRGB.
    pub fn set_image(&mut self, image: &DecodedImage, transform: &ColorTransform) {
        let extent = wgpu::Extent3d {
            width: image.width.max(1),
            height: image.height.max(1),
            depth_or_array_layers: 1,
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nitid image"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: if transform.is_identity {
                // The hardware linearises sRGB on sampling, which is both free
                // and filtered correctly.
                wgpu::TextureFormat::Rgba8UnormSrgb
            } else {
                // The shader needs the stored values untouched so it can push
                // them through the profile's curves.
                wgpu::TextureFormat::Rgba8Unorm
            },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            texture.as_image_copy(),
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.bytes_per_row()),
                rows_per_image: Some(extent.height),
            },
            extent,
        );

        self.write_curves(transform);
        self.queue.write_buffer(
            &self.colour,
            0,
            bytemuck::bytes_of(&ColourUniform::new(transform, self.config.format.is_srgb())),
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nitid image bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.curves_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.curve_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.colour.as_entire_binding(),
                },
            ],
        });

        self.upload = Some(Upload { bind_group });
    }

    /// Upload the sampled tone curves the shader reads.
    fn write_curves(&self, transform: &ColorTransform) {
        let halves: Vec<half::f16> = transform.decode.iter().map(|value| half::f16::from_f32(*value)).collect();

        self.queue.write_texture(
            self.curves.as_image_copy(),
            bytemuck::cast_slice(&halves),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((CURVE_SAMPLES * 2) as u32),
                rows_per_image: Some(3),
            },
            wgpu::Extent3d {
                width: CURVE_SAMPLES as u32,
                height: 3,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draw one frame.
    ///
    /// A lost or outdated swapchain is reconfigured and the frame skipped: the
    /// next redraw request paints it. Out of memory is fatal and propagates.
    pub fn render(&mut self, shown: Option<(&View, Orientation)>) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            // Suboptimal still presents; reconfiguring restores the fast path
            // for the frames after this one.
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.surface.configure(&self.device, &self.config);
                frame
            }
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            // Nothing is on screen to draw for, or the frame simply is not
            // ready: skip it and wait for the next redraw request.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };

        if let Some((view, orientation)) = shown {
            let placement = Placement::new(view, self.size(), orientation);
            self.queue.write_buffer(&self.uniforms, 0, placement.as_bytes());
        }

        let target = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nitid frame") });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nitid image pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if let Some(upload) = &self.upload {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &upload.bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }
}

/// Choose a swapchain configuration.
///
/// An sRGB surface format is preferred so the shader can write linear values
/// and let the display hardware encode them. v0.6.0 replaces this choice with
/// an HDR-aware one; keeping the selection here is what makes that possible.
fn configure(capabilities: &wgpu::SurfaceCapabilities, size: (u32, u32)) -> wgpu::SurfaceConfiguration {
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(|format| format.is_srgb())
        .unwrap_or(capabilities.formats[0]);

    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        // This field is the whole reason nitid configures its own surface:
        // v0.6.0 replaces `Auto` with `Bt2100Pq` on `Rgb10a2Unorm` after
        // querying `SurfaceCapabilities::format_capabilities`.
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: size.0.max(1),
        height: size.1.max(1),
        // Vsync: a viewer showing a still image has no reason to tear or to
        // spend a GPU budget racing the display.
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: capabilities.alpha_modes[0],
        view_formats: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shader is compiled by the driver at startup, so a mistake in it is
    /// a crash on launch rather than a build failure. Parsing it here turns
    /// that back into a test — it has already caught a reserved keyword and a
    /// byte-order mark left by an editor.
    #[test]
    fn the_shader_compiles() {
        let source = include_str!("gpu/shader.wgsl");
        assert!(!source.starts_with('\u{feff}'), "the shader begins with a byte-order mark, which WGSL rejects");

        let module = naga::front::wgsl::parse_str(source).expect("the shader should parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .expect("the shader should validate");
    }

    #[test]
    fn the_colour_uniform_matches_the_shader_layout() {
        // mat3x3 with 16-byte aligned columns, then two u32 and padding.
        assert_eq!(std::mem::size_of::<ColourUniform>(), 64);
    }

    #[test]
    fn an_identity_transform_asks_the_shader_to_do_nothing() {
        let uniform = ColourUniform::new(&ColorTransform::identity(), true);
        assert_eq!(uniform.convert, 0);
        assert_eq!(uniform.encode_srgb, 0);
    }

    #[test]
    fn a_conversion_onto_an_srgb_surface_leaves_encoding_to_the_hardware() {
        let transform = ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb());
        let uniform = ColourUniform::new(&transform, true);

        assert_eq!(uniform.convert, 1);
        // An sRGB surface encodes on write; doing it in the shader too would
        // apply the curve twice and wash the image out.
        assert_eq!(uniform.encode_srgb, 0);
    }

    #[test]
    fn a_conversion_onto_a_linear_surface_encodes_in_the_shader() {
        let transform = ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb());
        let uniform = ColourUniform::new(&transform, false);

        assert_eq!(uniform.convert, 1);
        assert_eq!(uniform.encode_srgb, 1);
    }

    #[test]
    fn placement_is_thirty_two_bytes() {
        // The shader's `Placement` must agree; a mismatch is silent corruption
        // of the framing rather than a compile error.
        assert_eq!(std::mem::size_of::<Placement>(), 32);
    }

    #[test]
    fn a_fitted_image_fills_the_window_without_offset() {
        let view = View::new((1000, 500), (1000, 500), 1.0);
        let placement = Placement::new(&view, (1000, 500), Orientation::Normal);

        assert_eq!(placement.half_size, [1.0, 1.0]);
        assert_eq!(placement.centre, [0.0, 0.0]);
    }

    #[test]
    fn panning_moves_the_centre_in_clip_space_with_y_inverted() {
        let mut view = View::new((2000, 2000), (1000, 1000), 1.0);
        view.set_actual();
        view.pan((100.0, 100.0));

        let placement = Placement::new(&view, (1000, 1000), Orientation::Normal);
        assert!(placement.centre[0] > 0.0);
        // Dragging down in screen pixels moves the image down, which is
        // negative in a y-up clip space.
        assert!(placement.centre[1] < 0.0);
    }

    #[test]
    fn every_orientation_matrix_preserves_area() {
        for orientation in [
            Orientation::Normal,
            Orientation::FlipHorizontal,
            Orientation::Rotate180,
            Orientation::FlipVertical,
            Orientation::Transpose,
            Orientation::Rotate90,
            Orientation::Transverse,
            Orientation::Rotate270,
        ] {
            let m = orientation_matrix(orientation);
            let determinant = m[0][0] * m[1][1] - m[0][1] * m[1][0];
            assert_eq!(determinant.abs(), 1.0, "{orientation:?} does not map the quad onto the whole texture");
        }
    }

    #[test]
    fn a_quarter_turn_exchanges_the_texture_axes() {
        // Rotate90 must sample across the texture's height for a corner that
        // moves along the quad's width.
        let m = orientation_matrix(Orientation::Rotate90);
        assert_eq!(m[0], [0.0, 1.0]);
        assert_eq!(m[1], [-1.0, 0.0]);
    }
}
