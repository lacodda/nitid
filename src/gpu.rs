//! The GPU side: device, swapchain, and the single pass that draws the image.
//!
//! nitid configures its own surface rather than delegating to a GUI framework,
//! because HDR output needs a surface format and colour space a framework
//! chooses for itself. See `docs/adr/0001-own-the-swapchain.md`. Which pair is
//! chosen, and when, lives in `hdr.rs`; this module holds it and follows the
//! display when it changes.

use std::sync::Arc;

use anyhow::{Context, Result};
use wgpu::util::DeviceExt;

use crate::color::{CURVE_SAMPLES, ColorTransform};
use crate::hdr::{self, Output};
use crate::image_source::{DecodedImage, Depth, Orientation};
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
    /// Non-zero when the surface carries extended-range linear light, where
    /// values above 1.0 are brighter than SDR white rather than an overflow to
    /// be clipped.
    extended_range: u32,
    _padding: u32,
}

impl ColourUniform {
    /// `depth` is the depth of the texture as uploaded — a sixteen-bit texture
    /// has no `*Srgb` variant, so the hardware cannot decode it on sampling
    /// and the shader's curves must, even when the transform itself would
    /// change nothing. The identity transform carries the sRGB curve for
    /// exactly this.
    fn new(transform: &ColorTransform, output: Output, depth: Depth) -> Self {
        let mut matrix = [[0.0; 4]; 3];
        for (row, values) in transform.matrix.iter().enumerate() {
            matrix[row][..3].copy_from_slice(values);
        }

        Self {
            matrix,
            convert: u32::from(!transform.is_identity || depth == Depth::Sixteen),
            // An sRGB surface format encodes for us on write. Anything else
            // expects the shader to have done it — except an extended-range
            // linear surface, which wants the light itself.
            encode_srgb: u32::from(!output.encodes_srgb() && !output.is_hdr()),
            extended_range: u32::from(output.is_hdr()),
            _padding: 0,
        }
    }
}

/// The image currently resident on the GPU.
struct Upload {
    bind_group: wgpu::BindGroup,
    /// Kept so an animation can write its next frame into the same texture
    /// rather than building a texture and bind group per frame.
    texture: wgpu::Texture,
    size: (u32, u32),
    /// The sample depth the texture was created for. A frame of another depth
    /// cannot be written into it — the formats differ.
    depth: Depth,
}

/// Device, surface, pipeline, and the texture being shown.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    /// Kept so the display's live HDR state can be asked again: the Windows
    /// HDR toggle moves while the viewer is open, and both the query and the
    /// surface capabilities need the adapter.
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// The format and colour space the swapchain is configured with, and what
    /// the pipeline was built to write.
    output: Output,
    pipeline: wgpu::RenderPipeline,
    /// Kept so the pipeline can be rebuilt when the surface format changes:
    /// a render pipeline is bound to the format of the target it writes.
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Tone curves sampled to linear light: three rows, one per channel.
    curves: wgpu::Texture,
    curves_view: wgpu::TextureView,
    curve_sampler: wgpu::Sampler,
    colour: wgpu::Buffer,
    /// The transform of the image currently up, kept so the colour uniform can
    /// be rewritten when the surface changes under it — the same picture needs
    /// different encoding on an SDR and an HDR surface.
    transform: ColorTransform,
    /// Whether the device can hold sixteen-bit normalised textures.
    ///
    /// `Rgba16Unorm` is an optional wgpu feature. DX12 — the backend nitid
    /// runs on — mandates the underlying format, so on Windows this is
    /// simply true; elsewhere a wide image is narrowed on upload rather than
    /// refused.
    wide_textures: bool,
    upload: Option<Upload>,
}

impl Renderer {
    /// Bring up a device on `window` and configure the swapchain.
    pub fn new(window: Arc<winit::window::Window>, size: (u32, u32)) -> Result<Self> {
        // Enumerating every backend costs over a hundred milliseconds of
        // startup, because each one loads its driver before being rejected.
        // On Windows only DX12 matters: it is the backend that reports the
        // HDR surface pairs (ADR 0013), so the others are work that can never
        // pay off.
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

        // Sixteen-bit textures are how a 10- or 12-bit image reaches the
        // screen at its own depth. Asked for only when the adapter offers
        // them, so a device that cannot is still a device.
        let wide_textures = adapter.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nitid device"),
            required_features: if wide_textures {
                wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            } else {
                wgpu::Features::empty()
            },
            // Downlevel limits keep integrated and older GPUs in scope; nothing
            // in the image path needs more than a texture and one draw call.
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("requesting a graphics device")?;

        crate::startup::milestone("device created");

        let capabilities = surface.get_capabilities(&adapter);
        let headroom = surface.display_hdr_info(&adapter).tone_map_headroom();
        let output = hdr::choose(&capabilities, headroom);
        report(output, headroom);
        let config = configure(&capabilities, output, size);
        surface.configure(&device, &config);

        let layout = bind_group_layout(&device);

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

        let pipeline = build_pipeline(&device, &pipeline_layout, &shader, output.format);

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
            contents: bytemuck::bytes_of(&ColourUniform::new(&ColorTransform::identity(), output, Depth::Eight)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Self {
            surface,
            adapter,
            device,
            queue,
            config,
            output,
            pipeline,
            shader,
            pipeline_layout,
            layout,
            sampler,
            uniforms,
            curves,
            curves_view,
            curve_sampler,
            colour,
            transform: ColorTransform::identity(),
            wide_textures,
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

    /// Whether the surface is currently configured for high dynamic range.
    ///
    /// The viewer polls the display only in this state: it is the one that can
    /// go stale without being announced, because turning HDR off in Windows
    /// sends no event to anybody.
    pub fn is_hdr(&self) -> bool {
        self.output.is_hdr()
    }

    /// Ask the display what it is doing now, and follow it.
    ///
    /// The HDR toggle in Windows moves while the viewer is open, and a window
    /// dragged to another monitor lands on a display with its own answer. Both
    /// arrive as ordinary window events rather than as anything the graphics
    /// API announces, so the question is asked again at those moments instead
    /// of being settled once at startup.
    ///
    /// Returns true when the swapchain was reconfigured, which is the caller's
    /// cue to redraw: the frames already queued were encoded for the old
    /// surface.
    pub fn follow_display(&mut self) -> bool {
        let capabilities = self.surface.get_capabilities(&self.adapter);
        let headroom = self.surface.display_hdr_info(&self.adapter).tone_map_headroom();
        let output = hdr::choose(&capabilities, headroom);
        if output == self.output {
            return false;
        }

        report(output, headroom);
        self.output = output;
        self.config = configure(&capabilities, output, (self.config.width, self.config.height));
        self.surface.configure(&self.device, &self.config);
        // A pipeline is built against the format it writes, so the format
        // changing means this one no longer applies.
        self.pipeline = build_pipeline(&self.device, &self.pipeline_layout, &self.shader, output.format);
        // The picture on screen was encoded for the surface that just went
        // away; the uniform says how, so it is rewritten for the new one.
        let depth = self.upload.as_ref().map_or(Depth::Eight, |upload| upload.depth);
        self.queue
            .write_buffer(&self.colour, 0, bytemuck::bytes_of(&ColourUniform::new(&self.transform, output, depth)));
        true
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

        // A wide image on a device without wide textures is narrowed here,
        // once, at upload — the picture still opens, at the depth an 8-bit
        // file would have had. Not met on Windows, where DX12 mandates the
        // format; kept so the one place that would hit it degrades instead
        // of refusing the file.
        let narrowed;
        let (pixels, depth): (&[u8], Depth) = match image.depth {
            Depth::Sixteen if !self.wide_textures => {
                narrowed = narrow_to_eight(&image.pixels);
                (&narrowed, Depth::Eight)
            }
            depth => (&image.pixels, depth),
        };

        let format = match (depth, transform.is_identity) {
            // The hardware linearises sRGB on sampling, which is both free
            // and filtered correctly.
            (Depth::Eight, true) => wgpu::TextureFormat::Rgba8UnormSrgb,
            // The shader needs the stored values untouched so it can push
            // them through the profile's curves.
            (Depth::Eight, false) => wgpu::TextureFormat::Rgba8Unorm,
            // No `*Srgb` variant exists at sixteen bits, so the shader's
            // curves decode this whatever the transform says — that is the
            // `depth` handed to `ColourUniform` below.
            (Depth::Sixteen, _) => wgpu::TextureFormat::Rgba16Unorm,
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nitid image"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(extent.width * 4 * depth.bytes()),
                rows_per_image: Some(extent.height),
            },
            extent,
        );

        self.transform = transform.clone();
        self.write_curves(transform);
        self.queue
            .write_buffer(&self.colour, 0, bytemuck::bytes_of(&ColourUniform::new(transform, self.output, depth)));

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

        self.upload = Some(Upload {
            bind_group,
            texture,
            size: (image.width, image.height),
            depth,
        });
    }

    /// Write new pixels into the texture already on screen.
    ///
    /// This is the frame tick of an animation: the texture, its format, the
    /// colour transform and the bind group all stand — the frames of one file
    /// share a size and a profile — so a frame costs one upload rather than
    /// the full `set_image`. False when nothing is up yet or the size does
    /// not match, in which case the caller's picture is wrong enough that a
    /// full `set_image` is the answer.
    pub fn update_pixels(&mut self, image: &DecodedImage) -> bool {
        let Some(upload) = &self.upload else {
            return false;
        };
        if upload.size != (image.width, image.height) || upload.depth != image.depth {
            return false;
        }

        let extent = wgpu::Extent3d {
            width: image.width.max(1),
            height: image.height.max(1),
            depth_or_array_layers: 1,
        };
        self.queue.write_texture(
            upload.texture.as_image_copy(),
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.bytes_per_row()),
                rows_per_image: Some(extent.height),
            },
            extent,
        );
        true
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

/// The bindings one image draw needs: the texture and its sampler, placement,
/// the tone curves and theirs, and the colour uniform.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
    })
}

/// Keep the top byte of each sixteen-bit sample.
///
/// The fallback for a device without wide textures: the same narrowing the
/// decoder itself used to do, moved to the one machine that needs it.
fn narrow_to_eight(pixels: &[u8]) -> Vec<u8> {
    pixels
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_ne_bytes(*pair).to_be_bytes()[0])
        .collect()
}

/// State the output signal for the startup report.
///
/// A screenshot of an HDR window is a standard-range image, so the one thing a
/// person cannot check by looking is whether high dynamic range is actually on.
fn report(output: Output, headroom: Option<f32>) {
    crate::startup::surface(&format!("{:?}", output.format), &format!("{:?}", output.color_space), headroom);
}

/// Build the swapchain configuration for a chosen output.
///
/// The `format` and `color_space` pair is the whole reason nitid configures
/// its own surface: `hdr::choose` picks it, and a framework-managed surface
/// would pick something else.
fn configure(capabilities: &wgpu::SurfaceCapabilities, output: Output, size: (u32, u32)) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: output.format,
        color_space: output.color_space,
        width: size.0.max(1),
        height: size.1.max(1),
        // Vsync: a viewer showing a still image has no reason to tear or to
        // spend a GPU budget racing the display.
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode: capabilities.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto),
        view_formats: vec![],
    }
}

/// Build the render pipeline for a target format.
///
/// A pipeline is bound to the format it writes, so switching the surface
/// between standard and high dynamic range means building this again.
fn build_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout, shader: &wgpu::ShaderModule, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("nitid image pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
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
    })
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

    /// The surface an SDR display gets: the hardware encodes on write.
    fn srgb_surface() -> Output {
        Output {
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            color_space: wgpu::SurfaceColorSpace::Auto,
        }
    }

    /// The surface an HDR display gets: extended-range linear light.
    fn hdr_surface() -> Output {
        Output {
            format: wgpu::TextureFormat::Rgba16Float,
            color_space: wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
        }
    }

    /// A surface with neither an sRGB format nor an HDR colour space, which
    /// leaves the encoding to the shader.
    fn plain_surface() -> Output {
        Output {
            format: wgpu::TextureFormat::Bgra8Unorm,
            color_space: wgpu::SurfaceColorSpace::Srgb,
        }
    }

    fn wide_gamut_transform() -> ColorTransform {
        ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb())
    }

    #[test]
    fn an_identity_transform_asks_the_shader_to_do_nothing() {
        let uniform = ColourUniform::new(&ColorTransform::identity(), srgb_surface(), Depth::Eight);
        assert_eq!(uniform.convert, 0);
        assert_eq!(uniform.encode_srgb, 0);
        assert_eq!(uniform.extended_range, 0);
    }

    #[test]
    fn a_conversion_onto_an_srgb_surface_leaves_encoding_to_the_hardware() {
        let uniform = ColourUniform::new(&wide_gamut_transform(), srgb_surface(), Depth::Eight);

        assert_eq!(uniform.convert, 1);
        // An sRGB surface encodes on write; doing it in the shader too would
        // apply the curve twice and wash the image out.
        assert_eq!(uniform.encode_srgb, 0);
    }

    #[test]
    fn a_plain_surface_is_encoded_by_the_shader() {
        // Neither the format nor the colour space encodes, so the shader must.
        // This holds for an untouched image too: before HDR the flag was tied
        // to the conversion, which would have written linear light to a
        // surface expecting sRGB — dark, and only on hardware nitid had not
        // met.
        assert_eq!(ColourUniform::new(&wide_gamut_transform(), plain_surface(), Depth::Eight).encode_srgb, 1);
        assert_eq!(ColourUniform::new(&ColorTransform::identity(), plain_surface(), Depth::Eight).encode_srgb, 1);
    }

    #[test]
    fn an_extended_range_surface_is_given_light_rather_than_an_encoding() {
        for transform in [ColorTransform::identity(), wide_gamut_transform()] {
            let uniform = ColourUniform::new(&transform, hdr_surface(), Depth::Eight);

            assert_eq!(uniform.extended_range, 1);
            // Encoding here would apply a transfer function the surface does
            // not expect; it wants the linear light itself.
            assert_eq!(uniform.encode_srgb, 0, "an HDR surface must not be handed encoded values");
        }
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

    /// Undo the sRGB transfer function, so a value read back from an `*Srgb`
    /// target can be compared with the light an HDR target holds directly.
    fn srgb_to_linear(value: f32) -> f32 {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Draw one image through the real pipeline into a texture of `format`,
    /// and read the result back as linear light.
    ///
    /// This is the shader itself, on a device, with the bindings the viewer
    /// uses — the arithmetic that decides whether an HDR surface shows the
    /// same picture an SDR one does. Nothing else in the suite would catch a
    /// transfer function applied once too often: the frame buffer is the only
    /// place that mistake becomes visible.
    ///
    /// `None` when no adapter can be had, which is a fact about the machine
    /// rather than a regression in the viewer.
    fn draw_offscreen(output: Output, transform: &ColorTransform, pixel: [u8; 4]) -> Option<[f32; 4]> {
        draw_offscreen_at(output, transform, &pixel, Depth::Eight)
    }

    fn draw_offscreen_at(output: Output, transform: &ColorTransform, pixel: &[u8], depth: Depth) -> Option<[f32; 4]> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let wide = adapter.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM);
        if depth == Depth::Sixteen && !wide {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nitid offscreen test device"),
            required_features: if wide {
                wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            } else {
                wgpu::Features::empty()
            },
            ..Default::default()
        }))
        .ok()?;

        let layout = bind_group_layout(&device);
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu/shader.wgsl").into()),
        });
        let pipeline = build_pipeline(&device, &pipeline_layout, &shader, output.format);

        let extent = wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        };

        // One pixel of image, in the texture format `set_image` would choose.
        let image = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: match (depth, transform.is_identity) {
                (Depth::Eight, true) => wgpu::TextureFormat::Rgba8UnormSrgb,
                (Depth::Eight, false) => wgpu::TextureFormat::Rgba8Unorm,
                (Depth::Sixteen, _) => wgpu::TextureFormat::Rgba16Unorm,
            },
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            image.as_image_copy(),
            pixel,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * depth.bytes()),
                rows_per_image: Some(1),
            },
            extent,
        );

        let curve_extent = wgpu::Extent3d {
            width: CURVE_SAMPLES as u32,
            height: 3,
            depth_or_array_layers: 1,
        };
        let curves = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: curve_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let halves: Vec<half::f16> = transform.decode.iter().map(|value| half::f16::from_f32(*value)).collect();
        queue.write_texture(
            curves.as_image_copy(),
            bytemuck::cast_slice(&halves),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((CURVE_SAMPLES * 2) as u32),
                rows_per_image: Some(3),
            },
            curve_extent,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let placement = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: Placement {
                half_size: [1.0, 1.0],
                centre: [0.0, 0.0],
                orientation: orientation_matrix(Orientation::Normal),
            }
            .as_bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let colour = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&ColourUniform::new(transform, output, depth)),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let image_view = image.create_view(&wgpu::TextureViewDescriptor::default());
        let curves_view = curves.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&image_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: placement.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&curves_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: colour.as_entire_binding(),
                },
            ],
        });

        // The target stands in for the swapchain texture.
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: output.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // 256 bytes is the row alignment a texture-to-buffer copy requires.
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
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
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            extent,
        );
        queue.submit(Some(encoder.finish()));

        readback.map_async(wgpu::MapMode::Read, .., |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;
        let bytes = readback.slice(..).get_mapped_range().ok()?.to_vec();

        // Whatever the surface format holds, the answer comes back as linear
        // light, so the two paths can be compared with each other.
        Some(match output.format {
            wgpu::TextureFormat::Rgba16Float => {
                let halves: &[half::f16] = bytemuck::cast_slice(&bytes[..8]);
                [halves[0].to_f32(), halves[1].to_f32(), halves[2].to_f32(), halves[3].to_f32()]
            }
            // An `*Srgb` target holds encoded values; undoing the encoding
            // gets back to the light the shader was working in.
            format if format.is_srgb() => std::array::from_fn(|index| {
                let value = f32::from(bytes[index]) / 255.0;
                if index == 3 { value } else { srgb_to_linear(value) }
            }),
            _ => std::array::from_fn(|index| f32::from(bytes[index]) / 255.0),
        })
    }

    /// The same picture must reach the eye the same way on either surface.
    ///
    /// This is the failure high dynamic range invites: a transfer function
    /// applied once too often, or not at all, leaves an image washed out or
    /// crushed while every unit test still passes. Drawing it both ways and
    /// comparing the light is the check that notices.
    #[test]
    fn a_standard_range_image_looks_the_same_on_an_hdr_surface() {
        let transform = ColorTransform::identity();
        // Mid grey: far enough from both ends that a wrong curve moves it a
        // long way, unlike black and white, which several mistakes leave
        // exactly where they were.
        let pixel = [128, 128, 128, 255];

        let Some(sdr) = draw_offscreen(srgb_surface(), &transform, pixel) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let hdr = draw_offscreen(hdr_surface(), &transform, pixel).expect("the adapter answered once already");

        for channel in 0..3 {
            assert!(
                (sdr[channel] - hdr[channel]).abs() < 0.01,
                "channel {channel}: the SDR surface shows {sdr:?} and the HDR one {hdr:?} - \
                 the same image must be the same light on either"
            );
        }

        // And it must be the light the image states, not merely a consistent
        // wrong answer on both paths: sRGB 128 is a little over a fifth of the
        // way up in linear light, not half.
        assert!(
            (hdr[0] - 0.2140).abs() < 0.01,
            "mid grey came out at {} rather than the 0.214 sRGB defines",
            hdr[0]
        );
    }

    /// Highlights above SDR white are what the extended-range surface is for.
    #[test]
    fn an_extended_range_surface_carries_light_brighter_than_white() {
        // A transform that scales past 1.0, standing in for a conversion whose
        // result leaves the box a standard-range surface can hold.
        let mut transform = ColorTransform::identity();
        transform.is_identity = false;
        transform.matrix = [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 2.0]];

        let Some(hdr) = draw_offscreen(hdr_surface(), &transform, [255, 255, 255, 255]) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        assert!(hdr[0] > 1.5, "white doubled should reach past SDR white rather than stop at it: {hdr:?}");

        // The same transform on a standard-range surface clips, because that
        // surface cannot carry it - which is what makes the HDR one worth
        // configuring at all.
        let sdr = draw_offscreen(srgb_surface(), &transform, [255, 255, 255, 255]).expect("the adapter answered already");
        assert!(sdr[0] <= 1.01, "a standard-range surface should clip at white: {sdr:?}");
    }

    /// A sixteen-bit texture must show the same light as the eight-bit one.
    ///
    /// The wide path has no `*Srgb` hardware decode — the shader's curves
    /// stand in for it — so this is where a curve applied once too often or
    /// not at all becomes visible. Mid grey again, because both endpoints
    /// survive most mistakes.
    #[test]
    fn a_wide_texture_shows_the_same_light_as_a_narrow_one() {
        let transform = ColorTransform::identity();
        let narrow = [128u8, 128, 128, 255];
        // The same grey as 16-bit samples: 128/255 of the full range.
        let sample = (128.0f32 / 255.0 * 65535.0 + 0.5) as u16;
        let mut wide = Vec::new();
        for value in [sample, sample, sample, u16::MAX] {
            wide.extend_from_slice(&value.to_ne_bytes());
        }

        let Some(eight) = draw_offscreen(srgb_surface(), &transform, narrow) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let Some(sixteen) = draw_offscreen_at(srgb_surface(), &transform, &wide, Depth::Sixteen) else {
            eprintln!("skipping: the adapter has no wide textures");
            return;
        };

        for channel in 0..3 {
            assert!(
                (eight[channel] - sixteen[channel]).abs() < 0.01,
                "channel {channel}: the narrow texture shows {eight:?} and the wide one {sixteen:?}"
            );
        }
    }
}
