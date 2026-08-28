//! The interface layer: egui drawn into a texture, then composited.
//!
//! egui chooses how to encode its output from whether the target format is an
//! sRGB one, and the extended-range surface nitid uses for HDR is not. Drawing
//! straight onto it was measured at 2.35 times too bright — a grey that should
//! read as 0.2140 of linear light came out at 0.5024.
//!
//! So egui gets an `Rgba8UnormSrgb` texture of its own, where its own choice is
//! the right one, and this module carries that texture to the surface with a
//! shader that asks the same question the image shader asks: light, encoded
//! values, or values untouched. See ADR 0017.

use wgpu::util::DeviceExt;

use crate::hdr::Output;

/// How the surface wants the interface encoded, matching `Surface` in
/// `overlay.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct SurfaceUniform {
    encode_srgb: u32,
    extended_range: u32,
    _padding: [u32; 2],
}

impl SurfaceUniform {
    fn new(output: Output) -> Self {
        Self {
            // The same rule the image uses: an `*Srgb` surface encodes on
            // write, an extended-range one wants light, anything else expects
            // the shader to have encoded.
            encode_srgb: u32::from(!output.encodes_srgb() && !output.is_hdr()),
            extended_range: u32::from(output.is_hdr()),
            _padding: [0; 2],
        }
    }
}

/// The texture egui paints into, and everything needed to put it on screen.
pub struct Overlay {
    renderer: egui_wgpu::Renderer,
    pipeline: wgpu::RenderPipeline,
    pipeline_layout: wgpu::PipelineLayout,
    shader: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    surface_uniform: wgpu::Buffer,
    /// The texture and its bind group, rebuilt when the window resizes.
    target: Option<Target>,
    size: (u32, u32),
}

struct Target {
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// One laid-out interface frame: what to draw, the textures it refers to, and
/// the scale it was laid out at. They are one answer from egui and are
/// meaningless apart, so they travel together.
pub struct Frame<'a> {
    pub jobs: &'a [egui::ClippedPrimitive],
    pub textures: &'a egui::TexturesDelta,
    pub pixels_per_point: f32,
}

/// The format egui draws into.
///
/// An sRGB format on purpose: it is the one egui is correct on, and the
/// hardware then hands linear light to the compositing shader for free.
const INTERFACE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

impl Overlay {
    pub fn new(device: &wgpu::Device, output: Output, size: (u32, u32)) -> Self {
        let renderer = egui_wgpu::Renderer::new(device, INTERFACE_FORMAT, egui_wgpu::RendererOptions::default());

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nitid interface bindings"),
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nitid interface pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nitid interface shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });

        let pipeline = build_pipeline(device, &pipeline_layout, &shader, output.format);

        // The interface is drawn at one texel per pixel and composited without
        // scaling, so nearest would do; linear costs the same and survives the
        // half-pixel offsets a fractional scale factor can produce.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nitid interface sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let surface_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("nitid interface surface"),
            contents: bytemuck::bytes_of(&SurfaceUniform::new(output)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Self {
            renderer,
            pipeline,
            pipeline_layout,
            shader,
            layout,
            sampler,
            surface_uniform,
            target: None,
            size,
        }
    }

    /// Follow the surface when it changes between standard and extended range.
    ///
    /// The compositing pipeline is built against the format it writes, and the
    /// uniform says how to encode for it; both have to be redone. The egui
    /// renderer does not, because it always draws into the same sRGB texture.
    pub fn follow_surface(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, output: Output) {
        self.pipeline = build_pipeline(device, &self.pipeline_layout, &self.shader, output.format);
        queue.write_buffer(&self.surface_uniform, 0, bytemuck::bytes_of(&SurfaceUniform::new(output)));
    }

    pub fn resize(&mut self, size: (u32, u32)) {
        if size != self.size {
            self.size = size;
            // Dropped rather than resized: the next frame builds one to fit.
            self.target = None;
        }
    }

    /// Draw `jobs` into the interface texture and composite it onto `screen`.
    ///
    /// Everything is recorded into `encoder`, which the caller submits along
    /// with the image pass, so the interface and the picture it sits on reach
    /// the screen in the same frame.
    pub fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        screen: &wgpu::TextureView,
        frame: Frame<'_>,
    ) -> Vec<wgpu::CommandBuffer> {
        let Frame {
            jobs,
            textures,
            pixels_per_point,
        } = frame;
        for (id, deltas) in &textures.set {
            for delta in deltas {
                self.renderer.update_texture(device, queue, *id, delta);
            }
        }

        let target = self
            .target
            .get_or_insert_with(|| Target::new(device, &self.layout, &self.sampler, &self.surface_uniform, self.size));

        let descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.size.0.max(1), self.size.1.max(1)],
            pixels_per_point,
        };
        let extra = self.renderer.update_buffers(device, queue, encoder, jobs, &descriptor);

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nitid interface pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    // Transparent, not the viewer's background: everything the
                    // interface does not cover has to leave the image showing.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut pass = pass.forget_lifetime();
            self.renderer.render(&mut pass, jobs, &descriptor);
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nitid interface composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: screen,
                    depth_slice: None,
                    resolve_target: None,
                    // Load, not clear: the image is already there.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &target.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }

        for id in &textures.free {
            self.renderer.free_texture(id);
        }

        extra
    }
}

impl Target {
    fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, surface_uniform: &wgpu::Buffer, size: (u32, u32)) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nitid interface"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: INTERFACE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nitid interface bindings"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: surface_uniform.as_entire_binding(),
                },
            ],
        });

        Self { view, bind_group }
    }
}

/// The compositing pipeline: one quad, premultiplied alpha over the image.
fn build_pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout, shader: &wgpu::ShaderModule, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("nitid interface pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // egui produces premultiplied alpha, so the source is added
                // whole and the destination is scaled by what is left of it.
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
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

    /// Draw a status bar through the real path and read the result back.
    ///
    /// A screenshot cannot answer this: `PrintWindow` does not capture a
    /// GPU-composited surface — measured, it returns the window's backdrop and
    /// shows the same grey for a build with the interface and one without.
    /// The frame buffer is the only place the interface can actually be seen.
    fn composite_over(surface: Output, picture: wgpu::Color) -> Option<Vec<[u8; 4]>> {
        composite_with(surface, picture, egui::Color32::from_rgb(20, 22, 28))
    }

    fn composite_with(surface: Output, picture: wgpu::Color, bar: egui::Color32) -> Option<Vec<[u8; 4]>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nitid interface test device"),
            ..Default::default()
        }))
        .ok()?;

        let (width, height) = (256u32, 128u32);
        let mut overlay = Overlay::new(&device, surface, (width, height));

        // A status bar along the bottom, laid out by egui itself.
        let context = egui::Context::default();
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        context.set_visuals(visuals);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32))),
            ..Default::default()
        };
        let output = context.run_ui(raw, |ui| {
            egui::Panel::bottom("status").frame(egui::Frame::new().fill(bar)).show(ui, |ui| {
                ui.label("status");
            });
        });
        let jobs = context.tessellate(output.shapes, output.pixels_per_point);
        assert!(!jobs.is_empty(), "egui produced nothing, so this would measure the clear colour");
        let mut textures = output.textures_delta;

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: surface.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // Stand in for the image pass: fill the target with a picture.
        {
            let _ = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(picture),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let extra = overlay.draw(
            &device,
            &queue,
            &mut encoder,
            &view,
            Frame {
                jobs: &jobs,
                textures: &textures,
                pixels_per_point: output.pixels_per_point,
            },
        );
        textures.clear();

        let row = (width * 4).next_multiple_of(256);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(extra.into_iter().chain(std::iter::once(encoder.finish())));
        readback.map_async(wgpu::MapMode::Read, .., |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;
        let bytes = readback.slice(..).get_mapped_range().ok()?.to_vec();

        // One column down the middle, which crosses the picture and the bar.
        Some(
            (0..height)
                .map(|y| {
                    let offset = (y * row + (width / 2) * 4) as usize;
                    [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]
                })
                .collect(),
        )
    }

    /// The measurement this whole module exists for: the interface has to be
    /// the colour egui asked for, not a gamma value mistaken for light.
    ///
    /// A mid grey (sRGB 128) reads as 0.2140 of linear light. Drawn into a
    /// non-sRGB texture, egui writes the gamma number instead and the same
    /// grey comes out at 0.5024 — measured, and 2.35 times too bright.
    #[test]
    fn the_interface_is_the_colour_egui_asked_for() {
        let Some(column) = fill_and_read(srgb_surface(), egui::Color32::from_rgb(128, 128, 128)) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        // The bar is opaque, so what is read back is the interface itself.
        let bar = column[column.len() - 4];
        // The surface is sRGB, so the stored byte is the sRGB encoding, and
        // 128 is what egui was given.
        for (channel, value) in bar[..3].iter().enumerate() {
            assert!(
                (i32::from(*value) - 128).abs() <= 2,
                "channel {channel} came out at {value} rather than 128 — the interface is being encoded twice, or not at all",
            );
        }
    }

    /// Draw a full-height bar of one flat colour and read the result back.
    fn fill_and_read(surface: Output, colour: egui::Color32) -> Option<Vec<[u8; 4]>> {
        composite_with(surface, wgpu::Color::BLACK, colour)
    }

    fn srgb_surface() -> Output {
        Output {
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            color_space: wgpu::SurfaceColorSpace::Auto,
        }
    }

    /// The two things the interface must get right at once: cover the picture
    /// where it draws, and leave it alone where it does not.
    #[test]
    fn the_interface_covers_its_own_bar_and_nothing_else() {
        // A strong red picture, so anything laid over it is unmistakable.
        let picture = wgpu::Color {
            r: 0.8,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let Some(column) = composite_over(srgb_surface(), picture) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        // Near the top the interface draws nothing, so the picture stands.
        let top = column[8];
        assert!(top[2] > 180, "the picture was dimmed where the interface draws nothing: {top:?}");
        assert!(top[1] < 40 && top[0] < 40, "something tinted the picture: {top:?}");

        // At the very bottom the status bar covers it.
        let bottom = column[column.len() - 4];
        assert!(bottom[2] < 80, "the status bar did not cover the picture: {bottom:?}");

        // And the two are actually different, or this test proves nothing.
        assert!(
            top[2] as i32 - bottom[2] as i32 > 100,
            "the bar and the picture read the same: {top:?} vs {bottom:?}"
        );
    }

    #[test]
    fn the_interface_shader_compiles() {
        let source = include_str!("overlay.wgsl");
        let module = naga::front::wgsl::parse_str(source).expect("the interface shader parses");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .expect("the interface shader validates");
    }

    #[test]
    fn the_surface_uniform_matches_the_shader_layout() {
        // Four 32-bit words, as `Surface` in the shader declares.
        assert_eq!(size_of::<SurfaceUniform>(), 16);
    }

    /// The interface answers the surface the same way the image does, or the
    /// two would disagree about what white is.
    #[test]
    fn the_interface_encodes_for_the_surface_the_way_the_image_does() {
        let srgb = Output {
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        let plain = Output {
            format: wgpu::TextureFormat::Bgra8Unorm,
            color_space: wgpu::SurfaceColorSpace::Srgb,
        };
        let hdr = Output {
            format: wgpu::TextureFormat::Rgba16Float,
            color_space: wgpu::SurfaceColorSpace::ExtendedSrgbLinear,
        };

        // An sRGB surface encodes on write, so the shader must not.
        let uniform = SurfaceUniform::new(srgb);
        assert_eq!((uniform.encode_srgb, uniform.extended_range), (0, 0));

        // A plain surface expects the shader to have encoded.
        let uniform = SurfaceUniform::new(plain);
        assert_eq!((uniform.encode_srgb, uniform.extended_range), (1, 0));

        // An extended-range surface wants light, never an encoding.
        let uniform = SurfaceUniform::new(hdr);
        assert_eq!((uniform.encode_srgb, uniform.extended_range), (0, 1));
    }
}
