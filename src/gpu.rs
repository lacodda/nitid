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
use crate::tiles::{self, Grid, Span, Tile};

mod overlay;
use crate::view::View;
pub use overlay::Overlay;

/// One frame of interface, ready to be composited.
///
/// Bundled rather than passed as four arguments because it travels together:
/// the jobs, the textures they refer to, and the scale they were laid out at
/// are one answer from egui and are meaningless apart.
pub struct Painted<'a> {
    pub layer: &'a mut Overlay,
    pub jobs: &'a [egui::ClippedPrimitive],
    pub textures: &'a egui::TexturesDelta,
    pub pixels_per_point: f32,
}

/// What shows through a transparent pixel.
///
/// The scene behind a photograph stays dark by decision, so that is the
/// default. The rest exist because judging a cut-out against one backdrop is
/// judging it against one background: a logo bound for a white page has to be
/// seen on white, and a checkerboard is how you tell "transparent" from "a
/// flat grey that happens to match the scene".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Backdrop {
    #[default]
    Scene,
    Checker,
    Black,
    White,
}

impl Backdrop {
    /// The next backdrop in the cycle the `B` key walks.
    pub fn next(self) -> Self {
        match self {
            Self::Scene => Self::Checker,
            Self::Checker => Self::Black,
            Self::Black => Self::White,
            Self::White => Self::Scene,
        }
    }

    /// What the status line calls it, and `None` for the default — which needs
    /// no name, because it is what the viewer always looks like.
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::Scene => None,
            Self::Checker => Some("checker"),
            Self::Black => Some("black"),
            Self::White => Some("white"),
        }
    }

    /// The number the shader switches on, matching `backdrop_at` in
    /// `shader.wgsl`.
    fn code(self) -> u32 {
        match self {
            Self::Scene => 0,
            Self::Checker => 1,
            Self::Black => 2,
            Self::White => 3,
        }
    }
}

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
/// two `vec2` fields ahead of it need no padding, and the two after it are
/// aligned already. The struct is 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Placement {
    half_size: [f32; 2],
    centre: [f32; 2],
    orientation: [[f32; 2]; 2],
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
}

impl Placement {
    /// Fold zoom, pan, and EXIF orientation into what the vertex stage needs.
    ///
    /// This is the whole image in one texture: the quad covers it and samples
    /// all of it.
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
            uv_scale: [1.0, 1.0],
            uv_offset: [0.0, 0.0],
        }
    }

    /// Narrow a whole-image placement down to one tile of it.
    ///
    /// `span` is the part of the image this tile draws and `inner` the part of
    /// its texture that holds those pixels, both as fractions — see
    /// `tiles.rs`. The tile's quad is the image's quad scaled to `span`, which
    /// keeps zoom and pan in one place: they shape the image rectangle, and
    /// each tile is a fixed fraction of whatever that rectangle became.
    ///
    /// `span` is in the image's own coordinates, and `half_size`/`centre` are
    /// already the screen's, so the orientation has to be applied to the span
    /// to bring the two into the same frame. A rotated image otherwise draws
    /// each tile correctly and puts it in the wrong place — measured at
    /// 224/255 away from the same picture drawn whole.
    fn for_tile(mut self, orientation: Orientation, span: Span, inner: Span) -> Self {
        let (span_width, span_height) = span.size();
        let (centre_x, centre_y) = span.centre();

        // Offset from the image's centre, in image coordinates and in the
        // -1..1 units the quad's half-size is stated in.
        let from_centre = [2.0 * centre_x - 1.0, 2.0 * centre_y - 1.0];
        let (on_screen, screen_width, screen_height) = to_screen(orientation, from_centre, span_width, span_height);

        // The image rectangle runs -half_size..half_size around `centre`, so
        // the tile's centre is that offset scaled by the rectangle's reach.
        // `on_screen` is already clip space, y-up.
        self.centre = [
            self.centre[0] + self.half_size[0] * on_screen[0],
            self.centre[1] + self.half_size[1] * on_screen[1],
        ];
        self.half_size = [self.half_size[0] * screen_width, self.half_size[1] * screen_height];

        let (inner_width, inner_height) = inner.size();
        self.uv_scale = [inner_width, inner_height];
        self.uv_offset = [inner.left, inner.top];
        self
    }

    fn as_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(self)
    }
}

/// Carry an offset and a size from the image's own axes onto the screen's.
///
/// A tile knows where it sits in the image; the quad it has to fill is stated
/// in clip space, which the orientation has already turned. `orientation_matrix`
/// runs the other way — screen corner to the texel it should show — so this
/// inverts it rather than restating the eight cases by hand, and the tests
/// check the two against each other.
///
/// `offset` is in the image's -1..1 units, y-down. The returned offset is clip
/// space, y-up. The returned size is the span's, with the axes exchanged when
/// the orientation exchanges them.
fn to_screen(orientation: Orientation, offset: [f32; 2], width: f32, height: f32) -> ([f32; 2], f32, f32) {
    // The shader computes `m * v` with `v = (corner.x, -corner.y)`, WGSL
    // reading `m` column-major. Every orientation matrix is orthogonal —
    // measured, including the four reflections, whose determinant is -1 and
    // whose transpose is still their inverse — so undoing it is a transpose
    // rather than a division. The test checks that against the shader's own
    // matrix rather than trusting the claim.
    let m = orientation_matrix(orientation);
    let v = [m[0][0] * offset[0] + m[0][1] * offset[1], m[1][0] * offset[0] + m[1][1] * offset[1]];
    // `v` is `(corner.x, -corner.y)`, so the y turns on the way back out.
    // Dropping this negation is what put a tiled picture inside out.
    let on_screen = [v[0], -v[1]];

    let (width, height) = if orientation.swaps_axes() { (height, width) } else { (width, height) };
    (on_screen, width, height)
}

/// The inverse of the EXIF transform, mapping quad corners to texture space.
///
/// Applying orientation here rather than by shuffling decoded bytes keeps a
/// rotated 60-megapixel photo free of a second full-size copy.
fn orientation_matrix(orientation: Orientation) -> [[f32; 2]; 2] {
    // The one statement of these eight matrices lives on `Orientation`, which
    // also composes them for the viewing rotation. Two copies would be two
    // places for a sign to drift, and the drift would only show on mirrored
    // orientations that no ordinary photograph carries.
    let matrix = orientation.matrix();
    [
        [f32::from(matrix[0][0]), f32::from(matrix[0][1])],
        [f32::from(matrix[1][0]), f32::from(matrix[1][1])],
    ]
}

/// Where the clipping zebra draws its two lines, as fractions of full scale.
///
/// A setting rather than a constant since v0.24.0: what counts as a blown
/// highlight depends on the camera and on what the picture is for, and the
/// photographer is the one who knows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Thresholds {
    pub high: f32,
    pub low: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            high: crate::config::DEFAULT_CLIP_HIGH,
            low: crate::config::DEFAULT_CLIP_LOW,
        }
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
    /// What shows through a transparent pixel; `Backdrop::code`.
    backdrop: u32,
    /// Non-zero while the clipping zebra is showing.
    zebra: u32,
    /// Non-zero when the texture hands the shader linear light rather than
    /// the file's stored values — an `*Srgb` texture, which the hardware
    /// linearises on sampling. The zebra undoes that to judge the values the
    /// file actually holds.
    sampled_is_linear: u32,
    /// At or above this fraction of full scale the zebra calls a highlight
    /// blown, and at or below `clip_low` it calls a shadow blocked. Both are
    /// judged on the file's own stored values (ADR 0019).
    ///
    /// These two occupy what used to be declared padding: WGSL rounds the
    /// struct's size up to its 16-byte alignment and `#[repr(C)]` does not,
    /// so the six `u32` after the 48-byte matrix come to 72 and these reach
    /// the 80 both languages agree on.
    clip_high: f32,
    clip_low: f32,
}

impl ColourUniform {
    /// `depth` is the depth of the texture as uploaded — a sixteen-bit texture
    /// has no `*Srgb` variant, so the hardware cannot decode it on sampling
    /// and the shader's curves must, even when the transform itself would
    /// change nothing. The identity transform carries the sRGB curve for
    /// exactly this.
    fn new(transform: &ColorTransform, output: Output, depth: Depth, backdrop: Backdrop, zebra: bool, thresholds: Thresholds) -> Self {
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
            backdrop: backdrop.code(),
            zebra: u32::from(zebra),
            // The one case the image is uploaded into an `*Srgb` texture:
            // eight bits with nothing to convert, where `set_image` picks
            // `Rgba8UnormSrgb` and the hardware linearises on sampling. The
            // same condition, written once here and once there.
            sampled_is_linear: u32::from(depth == Depth::Eight && transform.is_identity),
            clip_high: thresholds.high,
            clip_low: thresholds.low,
        }
    }
}

/// One piece of the image on the GPU, with everything a draw call needs.
///
/// An image that fits a texture is a single one of these and costs exactly
/// what the untiled renderer cost.
struct TileUpload {
    bind_group: wgpu::BindGroup,
    /// Kept so an animation can write its next frame into the same texture
    /// rather than building a texture and bind group per frame.
    texture: wgpu::Texture,
    /// This tile's own placement uniforms. A tile needs its own quad and its
    /// own texture inset, so the single shared buffer of the untiled
    /// renderer cannot serve them all within one pass; at 48 bytes each and
    /// a handful of tiles, a buffer per tile is cheaper than the dynamic
    /// offsets it would otherwise take.
    placement: wgpu::Buffer,
    tile: Tile,
}

/// The image currently resident on the GPU.
struct Upload {
    tiles: Vec<TileUpload>,
    size: (u32, u32),
    /// The sample depth the textures were created for. A frame of another
    /// depth cannot be written into them — the formats differ.
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
    /// Tone curves sampled to linear light: three rows, one per channel.
    curves: wgpu::Texture,
    curves_view: wgpu::TextureView,
    curve_sampler: wgpu::Sampler,
    colour: wgpu::Buffer,
    /// The transform of the image currently up, kept so the colour uniform can
    /// be rewritten when the surface changes under it — the same picture needs
    /// different encoding on an SDR and an HDR surface.
    transform: ColorTransform,
    /// What shows through a transparent pixel. Held here for the same reason
    /// as the transform: it outlives any one image and has to be restated
    /// whenever the uniform is rewritten.
    backdrop: Backdrop,
    /// Whether the clipping zebra is showing.
    ///
    /// Lives beside the backdrop and for the same reason: it is a viewing
    /// choice that outlives any one image, so it is restated every time the
    /// colour uniform is rewritten.
    zebra: bool,
    /// Where the zebra draws its lines. Held here for the same reason as the
    /// backdrop: a viewing choice that outlives any one image.
    thresholds: Thresholds,
    /// Whether the device can hold sixteen-bit normalised textures.
    ///
    /// `Rgba16Unorm` is an optional wgpu feature. DX12 — the backend nitid
    /// runs on — mandates the underlying format, so on Windows this is
    /// simply true; elsewhere a wide image is narrowed on upload rather than
    /// refused.
    wide_textures: bool,
    /// The longest side a texture may have, and so the size an image is cut
    /// into pieces at. Taken from the device; see [`tile_limit`].
    tile_limit: u32,
    upload: Option<Upload>,
}

/// Decide how an image is cut up, and refuse the result if it still would not
/// fit the device.
///
/// `cut_at` is the limit tiles are made to, normally the device's own but
/// lowerable for tests; `device_limit` is what the driver will actually
/// accept. Kept apart, and checked here rather than trusted, because the cost
/// of handing wgpu an oversize texture is not a bad frame but the whole
/// viewer: `create_texture` cannot return an error, so it reports through the
/// device's error handler, which by default panics.
///
/// `None` when no cut satisfies the device, which cannot happen with a limit
/// taken from that device and is a bug rather than a file if it does.
fn plan_tiles(size: (u32, u32), cut_at: u32, device_limit: u32) -> Option<Grid> {
    let grid = Grid::new(size, cut_at);
    let fits = grid
        .tiles()
        .iter()
        .all(|tile| tile.padded_width <= device_limit && tile.padded_height <= device_limit);
    fits.then_some(grid)
}

/// The side limit to cut images at.
///
/// Normally the device's own, which on the hardware this runs on is 16384.
/// `NITID_TILE_LIMIT` lowers it, which is the only way to exercise the tiled
/// path in a test: no fixture is 16384 pixels wide, and one that was would
/// make the suite unbearable. It never raises the limit past what the device
/// will accept, because a texture over that limit is exactly the silent
/// failure this module exists to prevent. Nothing in normal use sets it.
fn tile_limit(device: &wgpu::Device) -> u32 {
    let device_limit = device.limits().max_texture_dimension_2d;
    std::env::var("NITID_TILE_LIMIT")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|limit| *limit > 0)
        .map_or(device_limit, |limit| limit.min(device_limit))
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

        let tile_limit = tile_limit(&device);
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
            contents: bytemuck::bytes_of(&ColourUniform::new(
                &ColorTransform::identity(),
                output,
                Depth::Eight,
                Backdrop::default(),
                false,
                Thresholds::default(),
            )),
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
            curves,
            curves_view,
            curve_sampler,
            colour,
            transform: ColorTransform::identity(),
            backdrop: Backdrop::default(),
            zebra: false,
            thresholds: Thresholds::default(),
            wide_textures,
            tile_limit,
            upload: None,
        })
    }

    /// The drawing surface size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// What shows through a transparent pixel.
    pub fn backdrop(&self) -> Backdrop {
        self.backdrop
    }

    /// Change what shows through a transparent pixel.
    ///
    /// Only the uniform is rewritten: the pixels, the pipeline and the
    /// textures are all untouched, so this costs one 32-byte write and the
    /// redraw the caller was going to ask for anyway.
    pub fn set_backdrop(&mut self, backdrop: Backdrop) {
        if self.backdrop == backdrop {
            return;
        }
        self.backdrop = backdrop;
        self.write_colour();
    }

    /// Whether the clipping zebra is showing.
    pub fn zebra(&self) -> bool {
        self.zebra
    }

    /// Move the lines the zebra judges by.
    ///
    /// Costs the same one uniform write the zebra itself does, so a
    /// photographer can drag the threshold and watch the marking follow.
    pub fn set_thresholds(&mut self, thresholds: Thresholds) {
        if self.thresholds == thresholds {
            return;
        }
        self.thresholds = thresholds;
        self.write_colour();
    }

    /// Show or hide the clipping zebra.
    ///
    /// Costs the same as the backdrop does — one uniform write — because the
    /// marking happens in the shader that was going to run anyway. Nothing is
    /// re-decoded and no pixels are touched, which is what lets it be turned
    /// on to check a highlight and off again without a pause.
    pub fn set_zebra(&mut self, zebra: bool) {
        if self.zebra == zebra {
            return;
        }
        self.zebra = zebra;
        self.write_colour();
    }

    /// Restate the colour uniform from what the renderer currently holds.
    ///
    /// The one place that builds it for the live state, so a caller cannot
    /// write a uniform that disagrees with the renderer's own fields.
    ///
    /// **Not covered by a test, and measured to be so.** Making this write the
    /// default backdrop while `set_backdrop` still stored the choice passed
    /// every test and the whole gate: the status line would say "white" and
    /// the screen would stay dark. `Renderer` needs a window, so nothing in
    /// the suite can hold one and ask it what it wrote — the same gap as
    /// `set_image`, recorded in the hub's backlog since v0.15.0. Splitting the
    /// build from the write was tried and changed nothing, because the split
    /// half is still a method on a `Renderer` no test can build. What does
    /// cover it is opening the viewer and pressing `B`, which is part of the
    /// release checklist rather than of `cargo test`.
    fn write_colour(&mut self) {
        let depth = self.upload.as_ref().map_or(Depth::Eight, |upload| upload.depth);
        let uniform = ColourUniform::new(&self.transform, self.output, depth, self.backdrop, self.zebra, self.thresholds);
        self.queue.write_buffer(&self.colour, 0, bytemuck::bytes_of(&uniform));
    }

    /// Point an existing interface layer at the surface as it is now.
    ///
    /// Called after `follow_display` reports a change: the layer's compositing
    /// pipeline is bound to the format it writes, exactly like the image one.
    pub fn follow_interface(&self, overlay: &mut Overlay) {
        overlay.follow_surface(&self.device, &self.queue, self.output);
    }

    /// Build the interface layer for this device and surface.
    ///
    /// Handed out rather than held here because the interface belongs to the
    /// application — what it draws is the viewer's business, and this module's
    /// business is only that it reaches the screen correctly encoded.
    pub fn interface(&self) -> Overlay {
        Overlay::new(&self.device, self.output, self.size())
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
        // away; the uniform says how, so it is rewritten for the new one —
        // through the same call the backdrop uses, so the two cannot come to
        // disagree about what the live state is. `self.output` is already the
        // new surface by here.
        self.write_colour();
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
        let size = (image.width.max(1), image.height.max(1));

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

        // Past the device's side limit an image becomes several textures. The
        // limit has to be respected rather than discovered: an oversize
        // `create_texture` has no `Result` to refuse with: it reports through
        // the device's error handler, whose default is a panic. Measured — a
        // 20000-pixel-wide file decoded in full and then took the viewer down
        // as it was about to appear. See ADR 0015.
        let Some(grid) = plan_tiles(size, self.tile_limit, self.device.limits().max_texture_dimension_2d) else {
            // Nothing can be drawn within this device's limits. Drop what is
            // up rather than hand the driver a texture it will reject.
            self.upload = None;
            return;
        };
        let bytes_per_pixel = 4 * depth.bytes() as usize;

        let mut uploads = Vec::with_capacity(grid.len());
        for tile in grid.tiles() {
            let extent = wgpu::Extent3d {
                width: tile.padded_width,
                height: tile.padded_height,
                depth_or_array_layers: 1,
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

            // The single-tile case is every ordinary photograph, and it
            // uploads the decoder's buffer directly — no copy, exactly as
            // before tiling existed.
            let owned;
            let data: &[u8] = if grid.is_single() {
                pixels
            } else {
                let Some(copied) = tiles::extract(pixels, size, tile, bytes_per_pixel) else {
                    // Short buffer: a decoder broke its own contract. Drop
                    // what is up rather than draw a torn picture.
                    self.upload = None;
                    return;
                };
                owned = copied;
                &owned
            };

            self.queue.write_texture(
                texture.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tile.padded_width * bytes_per_pixel as u32),
                    rows_per_image: Some(tile.padded_height),
                },
                extent,
            );

            let placement = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("nitid placement"),
                size: size_of::<Placement>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

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
                        resource: placement.as_entire_binding(),
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

            uploads.push(TileUpload {
                bind_group,
                texture,
                placement,
                tile: *tile,
            });
        }

        self.transform = transform.clone();
        self.write_curves(transform);
        self.queue.write_buffer(
            &self.colour,
            0,
            bytemuck::bytes_of(&ColourUniform::new(transform, self.output, depth, self.backdrop, self.zebra, self.thresholds)),
        );

        self.upload = Some(Upload { tiles: uploads, size, depth });
    }

    /// Write new pixels into the texture already on screen.
    ///
    /// This is the frame tick of an animation: the textures, their format, the
    /// colour transform and the bind groups all stand — the frames of one file
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

        let bytes_per_pixel = 4 * image.depth.bytes() as usize;
        for piece in &upload.tiles {
            let tile = &piece.tile;
            let extent = wgpu::Extent3d {
                width: tile.padded_width,
                height: tile.padded_height,
                depth_or_array_layers: 1,
            };

            // One tile is the whole picture, which is every animation the
            // viewer has met: a GIF past the texture limit is possible but
            // has never turned up, and it costs a copy per tile per frame
            // rather than being refused.
            let owned;
            let data: &[u8] = if upload.tiles.len() == 1 {
                &image.pixels
            } else {
                let Some(copied) = tiles::extract(&image.pixels, upload.size, tile, bytes_per_pixel) else {
                    return false;
                };
                owned = copied;
                &owned
            };

            self.queue.write_texture(
                piece.texture.as_image_copy(),
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tile.padded_width * bytes_per_pixel as u32),
                    rows_per_image: Some(tile.padded_height),
                },
                extent,
            );
        }
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
    pub fn render(&mut self, shown: Option<(&View, Orientation)>, interface: Option<Painted<'_>>) -> Result<()> {
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

        let target = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.draw_into(&target, self.size(), shown);

        // The interface goes into the same encoder, so it and the picture it
        // sits on reach the screen in one frame rather than as two.
        let extra = match interface {
            Some(interface) => interface.layer.draw(
                &self.device,
                &self.queue,
                &mut encoder,
                &target,
                overlay::Frame {
                    jobs: interface.jobs,
                    textures: interface.textures,
                    pixels_per_point: interface.pixels_per_point,
                },
            ),
            None => Vec::new(),
        };

        self.queue.submit(extra.into_iter().chain(std::iter::once(encoder.finish())));
        self.queue.present(frame);
        Ok(())
    }

    /// Record the image pass into a fresh encoder, drawing onto `target`.
    ///
    /// Split out of `render` so a test can point the same code at a texture it
    /// reads back: the swapchain is the one thing an offscreen test cannot
    /// have, and everything worth checking — where each tile lands, what it
    /// samples, how many draws it takes — is on this side of that line.
    fn draw_into(&self, target: &wgpu::TextureView, size: (u32, u32), shown: Option<(&View, Orientation)>) -> wgpu::CommandEncoder {
        if let (Some((view, orientation)), Some(upload)) = (shown, self.upload.as_ref()) {
            let whole = Placement::new(view, size, orientation);
            for piece in &upload.tiles {
                let placement = whole.for_tile(orientation, piece.tile.span(upload.size), piece.tile.inner_uv());
                self.queue.write_buffer(&piece.placement, 0, placement.as_bytes());
            }
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("nitid frame") });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("nitid image pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
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
                // One draw call per tile, which for every image that fits a
                // texture is one draw call in total.
                for piece in &upload.tiles {
                    pass.set_bind_group(0, &piece.bind_group, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }

        encoder
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

    /// A transparent pixel must actually show the chosen backdrop. Read back
    /// from the frame buffer rather than compared against the constant that
    /// produced it: the question is what reaches the screen, and a uniform
    /// that never arrives would pass any check made of its own value.
    #[test]
    fn a_transparent_pixel_shows_the_backdrop_that_was_chosen() {
        let clear = [0u8, 0, 0, 0];
        let Some(scene) = draw_offscreen_on(srgb_surface(), &ColorTransform::identity(), &clear, Depth::Eight, Backdrop::Scene) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        let black = draw_offscreen_on(srgb_surface(), &ColorTransform::identity(), &clear, Depth::Eight, Backdrop::Black).expect("an adapter");
        let white = draw_offscreen_on(srgb_surface(), &ColorTransform::identity(), &clear, Depth::Eight, Backdrop::White).expect("an adapter");

        // Black is black, white is white, and the scene is the dark grey the
        // viewer has always used.
        assert!(black[0] < 0.01, "the black backdrop came out at {}", black[0]);
        assert!(white[0] > 0.99, "the white backdrop came out at {}", white[0]);
        assert!(scene[0] > 0.05 && scene[0] < 0.15, "the scene backdrop came out at {}", scene[0]);

        // And they are actually different, or this test proves nothing.
        assert!(white[0] - black[0] > 0.9, "the backdrops read the same");
    }

    /// The checkerboard has to actually be a checkerboard: two greys
    /// alternating in squares of a fixed size on screen.
    ///
    /// Drawn across a row wider than one square, because it is a function of
    /// the screen position and a one-pixel target can only ever show one
    /// square of it — which would pass as a flat colour.
    #[test]
    fn the_checkerboard_alternates_in_squares_of_a_fixed_size() {
        // Wide enough for three squares at CHECKER_SIZE = 12.
        let width = 36;
        let clear = [0u8, 0, 0, 0];
        let Some(row) = draw_row_offscreen(srgb_surface(), &ColorTransform::identity(), &clear, Depth::Eight, Backdrop::Checker, width) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        // Two distinct greys, and no more than two.
        let mut values: Vec<f32> = row.iter().map(|pixel| pixel[0]).collect();
        values.sort_by(f32::total_cmp);
        values.dedup_by(|a, b| (*a - *b).abs() < 0.001);
        assert_eq!(values.len(), 2, "the checkerboard showed {} distinct greys: {values:?}", values.len());

        // Both dark: this sits beside the viewer's own dark scene, and a
        // checker that glares would be the brightest thing on screen.
        for value in &values {
            assert!(
                *value < 0.2,
                "a checker square came back at {value}, which is too bright to sit beside the scene"
            );
        }

        // The squares are twelve pixels wide: the first pixel of each square
        // matches the first pixel of the square two along, and differs from
        // its neighbour.
        assert!((row[0][0] - row[24][0]).abs() < 0.001, "squares two apart differ, so the period is not 12");
        assert!(
            (row[0][0] - row[12][0]).abs() > 0.01,
            "adjacent squares are the same colour, so there is no pattern"
        );
        // And within one square nothing changes.
        assert!((row[0][0] - row[11][0]).abs() < 0.001, "a square is not flat across its own width");
    }

    /// An opaque pixel must not be touched by any of it: the backdrop is what
    /// shows *through* transparency, not a tint over the picture.
    #[test]
    fn the_backdrop_leaves_an_opaque_pixel_alone() {
        let opaque = [128u8, 128, 128, 255];
        let Some(scene) = draw_offscreen_on(srgb_surface(), &ColorTransform::identity(), &opaque, Depth::Eight, Backdrop::Scene) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        for backdrop in [Backdrop::Checker, Backdrop::Black, Backdrop::White] {
            let drawn = draw_offscreen_on(srgb_surface(), &ColorTransform::identity(), &opaque, Depth::Eight, backdrop).expect("an adapter");
            assert!(
                (drawn[0] - scene[0]).abs() < 0.005,
                "{backdrop:?} changed an opaque pixel: {} vs {}",
                drawn[0],
                scene[0],
            );
        }
    }

    /// The `B` key walks all four and comes back, so it can be pressed
    /// forever without reaching a state it cannot leave.
    #[test]
    fn the_backdrop_cycle_visits_each_one_and_returns() {
        let mut seen = Vec::new();
        let mut backdrop = Backdrop::default();
        for _ in 0..4 {
            seen.push(backdrop);
            backdrop = backdrop.next();
        }
        assert_eq!(backdrop, Backdrop::default(), "the cycle did not come back to the start");
        for expected in [Backdrop::Scene, Backdrop::Checker, Backdrop::Black, Backdrop::White] {
            assert!(seen.contains(&expected), "{expected:?} is not in the cycle");
        }
    }

    /// Only the default goes unnamed in the status line — it is what the
    /// viewer always looks like, so saying so would be noise.
    #[test]
    fn every_backdrop_but_the_default_says_what_it_is() {
        assert_eq!(Backdrop::Scene.name(), None);
        for named in [Backdrop::Checker, Backdrop::Black, Backdrop::White] {
            assert!(named.name().is_some(), "{named:?} has no name for the status line");
        }
    }

    #[test]
    fn the_colour_uniform_matches_the_shader_layout() {
        // mat3x3 with 16-byte aligned columns (48 bytes), then six u32 and
        // the two zebra thresholds, which come to the struct's own 16-byte
        // alignment exactly.
        assert_eq!(std::mem::size_of::<ColourUniform>(), 80);
        assert_eq!(std::mem::size_of::<ColourUniform>() % 16, 0, "WGSL rounds the struct up; the two must agree");
    }

    /// The thresholds the settings choose are the ones the shader is handed.
    ///
    /// They travel in what used to be padding, which is exactly the kind of
    /// change that compiles and silently sends zeroes: a zebra told to mark
    /// everything at or above 0.0 would paint the whole picture.
    #[test]
    fn the_zebra_thresholds_reach_the_shader() {
        let uniform = ColourUniform::new(
            &ColorTransform::identity(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            true,
            Thresholds { high: 0.9, low: 0.1 },
        );
        assert_eq!(uniform.clip_high, 0.9);
        assert_eq!(uniform.clip_low, 0.1);

        // And the default is the pair the zebra judged by before it was a
        // setting, not zero.
        let default = ColourUniform::new(
            &ColorTransform::identity(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            true,
            Thresholds::default(),
        );
        assert_eq!(default.clip_high, crate::config::DEFAULT_CLIP_HIGH);
        assert_eq!(default.clip_low, crate::config::DEFAULT_CLIP_LOW);
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

    /// The red and blue of a pixel read back from a test surface.
    ///
    /// The surfaces here are `Bgra8*`, so the bytes arrive blue first. Reading
    /// them as RGB is exactly the mistake that made the zebra tests report a
    /// red mark as blue and sent this stage chasing a shader bug that did not
    /// exist — the existing colour tests never caught it because every colour
    /// they check is grey, where the order does not show.
    fn red_of(pixel: [f32; 4]) -> f32 {
        pixel[2]
    }

    fn blue_of(pixel: [f32; 4]) -> f32 {
        pixel[0]
    }

    fn wide_gamut_transform() -> ColorTransform {
        ColorTransform::new(&moxcms::ColorProfile::new_display_p3(), &moxcms::ColorProfile::new_srgb())
    }

    /// The zebra marks a blown highlight, and marking is visible: some pixels
    /// of the row carry the mark and some do not, which is what a hatch is.
    ///
    /// Read back from a real frame, because the marking happens in the shader
    /// and nothing on the Rust side can be asked whether it worked — the same
    /// reason the colour tests draw rather than compute.
    #[test]
    fn the_zebra_marks_a_blown_highlight() {
        let white = [255u8, 255, 255, 255];
        let Some(row) = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &white, Depth::Eight, 64) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        // The mark is red, so a striped row has pixels where red leads and
        // pixels where the white picture stands.
        let marked = row.iter().filter(|pixel| red_of(**pixel) > blue_of(**pixel) + 0.2).count();
        let plain = row.iter().filter(|pixel| blue_of(**pixel) > 0.8).count();

        assert!(marked > 0, "a blown highlight was not marked at all");
        assert!(plain > 0, "the mark covered the whole picture rather than striping it");
    }

    /// A picture with nothing clipped is left alone. Without this the test
    /// above would pass on a shader that painted every pixel red.
    #[test]
    fn the_zebra_leaves_an_unclipped_picture_alone() {
        let mid = [128u8, 128, 128, 255];
        let Some(with) = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &mid, Depth::Eight, 64) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let without = draw_row_offscreen(srgb_surface(), &ColorTransform::identity(), &mid, Depth::Eight, Backdrop::default(), 64)
            .expect("the adapter answered a moment ago");

        for (index, (a, b)) in with.iter().zip(&without).enumerate() {
            for channel in 0..3 {
                assert!(
                    (a[channel] - b[channel]).abs() < 0.01,
                    "pixel {index} channel {channel} changed with the zebra on: {a:?} vs {b:?}",
                );
            }
        }
    }

    /// A blocked shadow is marked too, and in the other colour: the two
    /// failures are opposite and must not look alike.
    #[test]
    fn a_blocked_shadow_is_marked_differently_from_a_blown_highlight() {
        let black = [0u8, 0, 0, 255];
        let white = [255u8, 255, 255, 255];
        let Some(shadows) = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &black, Depth::Eight, 64) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let highlights = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &white, Depth::Eight, 64).expect("the adapter answered a moment ago");

        // The mark itself in each row: the pixel furthest from neutral in the
        // direction its mark is coloured. Picking the *brightest* pixel finds
        // the unmarked picture instead — white is brighter than the red mark
        // laid over it, which is what this assertion first reported.
        let shadow_mark = shadows
            .iter()
            .copied()
            .max_by(|a, b| (blue_of(*a) - red_of(*a)).total_cmp(&(blue_of(*b) - red_of(*b))))
            .expect("the row is not empty");
        let highlight_mark = highlights
            .iter()
            .copied()
            .max_by(|a, b| (red_of(*a) - blue_of(*a)).total_cmp(&(red_of(*b) - blue_of(*b))))
            .expect("the row is not empty");

        assert!(
            blue_of(shadow_mark) > red_of(shadow_mark) + 0.1,
            "a blocked shadow was not marked in blue: {shadow_mark:?}",
        );
        assert!(
            red_of(highlight_mark) > blue_of(highlight_mark) + 0.1,
            "a blown highlight was not marked in red: {highlight_mark:?}",
        );
    }

    /// The decision ADR 0019 records, applied to the zebra: it marks what the
    /// *file* clipped, not what this display cannot reproduce.
    ///
    /// A mid-grey in a wide-gamut space converts to something well inside an
    /// sRGB display's range and clips nothing; a value at full scale in the
    /// file is clipped whatever the display does with it. The transform must
    /// not change the answer.
    #[test]
    fn the_zebra_judges_the_file_rather_than_the_display() {
        let mid = [128u8, 128, 128, 255];
        let Some(row) = draw_row_with_zebra(srgb_surface(), &wide_gamut_transform(), &mid, Depth::Eight, 64) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };

        // Nothing marked: the file's own value is nowhere near either end,
        // however the conversion moves it.
        let marked = row.iter().filter(|pixel| red_of(**pixel) > blue_of(**pixel) + 0.2).count();
        assert_eq!(marked, 0, "a pixel the file did not clip was marked because of the display");

        // And a value the file did clip is still marked through the same
        // transform, or the test above would pass on a zebra that never fires.
        let white = [255u8, 255, 255, 255];
        let clipped = draw_row_with_zebra(srgb_surface(), &wide_gamut_transform(), &white, Depth::Eight, 64).expect("the adapter answered a moment ago");
        assert!(
            clipped.iter().any(|pixel| red_of(*pixel) > blue_of(*pixel) + 0.2),
            "a highlight the file clipped went unmarked once a transform was involved",
        );
    }

    /// A sixteen-bit file reaches the shader as raw samples rather than
    /// linearised ones, so the zebra has to read the right numbers on both
    /// paths. Full scale is clipped at either depth.
    #[test]
    fn the_zebra_reads_a_sixteen_bit_file_too() {
        let full = 0xffffu16.to_ne_bytes();
        let pixel: Vec<u8> = [full, full, full, full].concat();
        let Some(row) = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &pixel, Depth::Sixteen, 64) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        assert!(
            row.iter().any(|pixel| red_of(*pixel) > blue_of(*pixel) + 0.2),
            "a sixteen-bit highlight at full scale went unmarked",
        );

        // And a mid-grey at the same depth is left alone.
        let half = 0x8000u16.to_ne_bytes();
        let opaque = 0xffffu16.to_ne_bytes();
        let mid: Vec<u8> = [half, half, half, opaque].concat();
        let quiet = draw_row_with_zebra(srgb_surface(), &ColorTransform::identity(), &mid, Depth::Sixteen, 64).expect("the adapter answered a moment ago");
        assert!(
            !quiet.iter().any(|pixel| red_of(*pixel) > blue_of(*pixel) + 0.2),
            "a sixteen-bit mid grey was marked as clipped",
        );
    }

    /// The zebra rides in the colour uniform, so it must not disturb the
    /// answers the surface already depends on.
    #[test]
    fn turning_the_zebra_on_changes_nothing_else_in_the_uniform() {
        let transform = wide_gamut_transform();
        let off = ColourUniform::new(&transform, srgb_surface(), Depth::Eight, Backdrop::default(), false, Thresholds::default());
        let on = ColourUniform::new(&transform, srgb_surface(), Depth::Eight, Backdrop::default(), true, Thresholds::default());

        assert_eq!((off.zebra, on.zebra), (0, 1), "the flag does not reach the shader");
        assert_eq!(off.convert, on.convert);
        assert_eq!(off.encode_srgb, on.encode_srgb);
        assert_eq!(off.extended_range, on.extended_range);
        assert_eq!(off.backdrop, on.backdrop);
        assert_eq!(off.matrix, on.matrix);
    }

    /// The shader is told whether the texture handed it linear light, and it
    /// is true for exactly one case: eight bits with nothing to convert, which
    /// is the `*Srgb` texture `set_image` picks. Getting this wrong reads the
    /// wrong numbers and marks the wrong pixels.
    #[test]
    fn the_shader_is_told_when_the_hardware_linearised() {
        let srgb_texture = ColourUniform::new(
            &ColorTransform::identity(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            true,
            Thresholds::default(),
        );
        assert_eq!(srgb_texture.sampled_is_linear, 1, "the one hardware-linearised case was not declared");

        // A conversion means a plain `Rgba8Unorm` texture: raw values.
        let converted = ColourUniform::new(
            &wide_gamut_transform(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            true,
            Thresholds::default(),
        );
        assert_eq!(converted.sampled_is_linear, 0, "a plain texture was reported as linearised");

        // Sixteen bits has no `*Srgb` variant at all.
        let wide = ColourUniform::new(
            &ColorTransform::identity(),
            srgb_surface(),
            Depth::Sixteen,
            Backdrop::default(),
            true,
            Thresholds::default(),
        );
        assert_eq!(wide.sampled_is_linear, 0, "a sixteen-bit texture was reported as linearised");
    }

    #[test]
    fn an_identity_transform_asks_the_shader_to_do_nothing() {
        let uniform = ColourUniform::new(
            &ColorTransform::identity(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            false,
            Thresholds::default(),
        );
        assert_eq!(uniform.convert, 0);
        assert_eq!(uniform.encode_srgb, 0);
        assert_eq!(uniform.extended_range, 0);
    }

    #[test]
    fn a_conversion_onto_an_srgb_surface_leaves_encoding_to_the_hardware() {
        let uniform = ColourUniform::new(
            &wide_gamut_transform(),
            srgb_surface(),
            Depth::Eight,
            Backdrop::default(),
            false,
            Thresholds::default(),
        );

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
        assert_eq!(
            ColourUniform::new(
                &wide_gamut_transform(),
                plain_surface(),
                Depth::Eight,
                Backdrop::default(),
                false,
                Thresholds::default()
            )
            .encode_srgb,
            1
        );
        assert_eq!(
            ColourUniform::new(
                &ColorTransform::identity(),
                plain_surface(),
                Depth::Eight,
                Backdrop::default(),
                false,
                Thresholds::default()
            )
            .encode_srgb,
            1
        );
    }

    #[test]
    fn an_extended_range_surface_is_given_light_rather_than_an_encoding() {
        for transform in [ColorTransform::identity(), wide_gamut_transform()] {
            let uniform = ColourUniform::new(&transform, hdr_surface(), Depth::Eight, Backdrop::default(), false, Thresholds::default());

            assert_eq!(uniform.extended_range, 1);
            // Encoding here would apply a transfer function the surface does
            // not expect; it wants the linear light itself.
            assert_eq!(uniform.encode_srgb, 0, "an HDR surface must not be handed encoded values");
        }
    }

    #[test]
    fn placement_is_forty_eight_bytes() {
        // The shader's `Placement` must agree; a mismatch is silent corruption
        // of the framing rather than a compile error. The two `vec2` fields
        // that carry a tile's texture inset took it from 32 bytes to 48.
        assert_eq!(std::mem::size_of::<Placement>(), 48);
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
        draw_offscreen_on(output, transform, pixel, depth, Backdrop::default())
    }

    fn draw_offscreen_on(output: Output, transform: &ColorTransform, pixel: &[u8], depth: Depth, backdrop: Backdrop) -> Option<[f32; 4]> {
        draw_row_offscreen(output, transform, pixel, depth, backdrop, 1).map(|row| row[0])
    }

    /// The same, with the clipping zebra on.
    ///
    /// A row rather than a pixel, because the zebra is a function of screen
    /// position: a one-pixel target lands on one stripe and says nothing
    /// about whether the hatch exists.
    fn draw_row_with_zebra(output: Output, transform: &ColorTransform, pixel: &[u8], depth: Depth, width: u32) -> Option<Vec<[f32; 4]>> {
        draw_row_offscreen_with(output, transform, pixel, depth, Backdrop::default(), width, true)
    }

    /// The same, drawn into a target `width` pixels wide, returning the row.
    ///
    /// A wider target is what makes the checkerboard visible at all: it is a
    /// function of the screen position, so a one-pixel target can only ever
    /// show one square of it.
    fn draw_row_offscreen(output: Output, transform: &ColorTransform, pixel: &[u8], depth: Depth, backdrop: Backdrop, width: u32) -> Option<Vec<[f32; 4]>> {
        draw_row_offscreen_with(output, transform, pixel, depth, backdrop, width, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row_offscreen_with(
        output: Output,
        transform: &ColorTransform,
        pixel: &[u8],
        depth: Depth,
        backdrop: Backdrop,
        width: u32,
        zebra: bool,
    ) -> Option<Vec<[f32; 4]>> {
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

        // The image spans the target: a one-pixel image in a wider window
        // covers one pixel and leaves the rest of the row showing the pass's
        // clear colour, which is not the shader's output at all. Measured —
        // it reads as a flat scene-coloured row with a single odd pixel.
        let pixels: Vec<u8> = pixel.repeat(width as usize);
        let extent = wgpu::Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        };

        // The image, in the texture format `set_image` would choose.
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
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4 * depth.bytes()),
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
        // Built through the production constructor rather than by hand, so a
        // mistake in how placement is assembled shows up here too: a view of
        // a 1x1 image in a 1x1 window fills the target exactly.
        let placement = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: Placement::new(&View::new((width, 1), (width, 1), 1.0), (width, 1), Orientation::Normal).as_bytes(),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let colour = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&ColourUniform::new(transform, output, depth, backdrop, zebra, Thresholds::default())),
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
        let target_extent = wgpu::Extent3d {
            width,
            height: 1,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: target_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: output.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // 256 bytes is the row alignment a texture-to-buffer copy requires, and
        // a wider row is rounded up to the next multiple of it. Stating this
        // wrongly is not an error: the copy succeeds and the values read back
        // are nonsense, which is how a measurement in v0.15.0 came back as all
        // zeroes while its test stayed green.
        let row_bytes = (width * 4).next_multiple_of(256);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(row_bytes),
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
                    bytes_per_row: Some(row_bytes),
                    rows_per_image: Some(1),
                },
            },
            target_extent,
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
        let stride = match output.format {
            wgpu::TextureFormat::Rgba16Float => 8,
            _ => 4,
        };
        Some(
            (0..width as usize)
                .map(|x| {
                    let at = x * stride;
                    match output.format {
                        wgpu::TextureFormat::Rgba16Float => {
                            let halves: &[half::f16] = bytemuck::cast_slice(&bytes[at..at + 8]);
                            [halves[0].to_f32(), halves[1].to_f32(), halves[2].to_f32(), halves[3].to_f32()]
                        }
                        // An `*Srgb` target holds encoded values; undoing the
                        // encoding gets back to the light the shader worked in.
                        format if format.is_srgb() => std::array::from_fn(|index| {
                            let value = f32::from(bytes[at + index]) / 255.0;
                            if index == 3 { value } else { srgb_to_linear(value) }
                        }),
                        _ => std::array::from_fn(|index| f32::from(bytes[at + index]) / 255.0),
                    }
                })
                .collect(),
        )
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

    /// Environment variables are a process-wide resource: a test that sets
    /// one races every other test in the binary. The sandbox suite learned
    /// this in v0.9.0, where "hang on purpose" leaked into four neighbours.
    static ENVIRONMENT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Draw a whole image at `limit` as the texture side limit, and read the
    /// target back as 8-bit RGBA rows.
    ///
    /// This drives the real pieces: `Grid` decides the cut, `tiles::extract`
    /// fills each texture, `Placement::for_tile` places each quad, and the
    /// production shader and pipeline draw them. Only the device and the
    /// swapchain are stood in for, because an offscreen test cannot have one.
    ///
    /// `None` when no adapter can be had, which is a fact about the machine.
    fn draw_tiled(image: &DecodedImage, limit: u32, view: &View, orientation: Orientation, target_size: (u32, u32)) -> Option<Vec<u8>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nitid tiling test device"),
            ..Default::default()
        }))
        .ok()?;

        let output = plain_surface();
        let transform = ColorTransform::identity();
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
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        // Tone curves, as `Renderer::write_curves` writes them.
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
        let curves_view = curves.create_view(&wgpu::TextureViewDescriptor::default());
        let curve_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
        let colour = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&ColourUniform::new(
                &transform,
                output,
                Depth::Eight,
                Backdrop::default(),
                false,
                Thresholds::default(),
            )),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let size = (image.width, image.height);
        let grid = Grid::new(size, limit);
        let whole = Placement::new(view, target_size, orientation);

        // Everything a draw needs, built exactly as `set_image` builds it.
        let mut pieces = Vec::new();
        for tile in grid.tiles() {
            let extent = wgpu::Extent3d {
                width: tile.padded_width,
                height: tile.padded_height,
                depth_or_array_layers: 1,
            };
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let data = tiles::extract(&image.pixels, size, tile, 4).expect("the fixture is well formed");
            queue.write_texture(
                texture.as_image_copy(),
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tile.padded_width * 4),
                    rows_per_image: Some(tile.padded_height),
                },
                extent,
            );

            let placement = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: whole.for_tile(orientation, tile.span(size), tile.inner_uv()).as_bytes(),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&texture_view),
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
                        resource: wgpu::BindingResource::Sampler(&curve_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: colour.as_entire_binding(),
                    },
                ],
            });
            pieces.push(bind_group);
        }

        let target_extent = wgpu::Extent3d {
            width: target_size.0,
            height: target_size.1,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: target_extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: output.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // A texture-to-buffer copy pads rows to 256 bytes. Getting this wrong
        // does not fail the copy loudly enough to notice: the probe that
        // designed this feature read all zeroes and nearly concluded that
        // tiling needed no overlap.
        let row = (target_size.0 * 4).next_multiple_of(256);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * target_size.1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
            for bind_group in &pieces {
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(target_size.1),
                },
            },
            target_extent,
        );
        queue.submit([encoder.finish()]);
        readback.map_async(wgpu::MapMode::Read, .., |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;
        let bytes = readback.slice(..).get_mapped_range().ok()?.to_vec();

        // Drop the row padding, so the caller compares pixels rather than
        // alignment.
        let mut out = Vec::with_capacity((target_size.0 * target_size.1 * 4) as usize);
        for y in 0..target_size.1 as usize {
            let start = y * row as usize;
            out.extend_from_slice(&bytes[start..start + (target_size.0 * 4) as usize]);
        }
        Some(out)
    }

    /// A picture with detail in both directions, so a tile placed wrongly
    /// shows up as a moved feature rather than as more of the same colour.
    fn test_picture(width: u32, height: u32) -> DecodedImage {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[(x * 255 / width.max(1)) as u8, (y * 255 / height.max(1)) as u8, ((x + y) % 256) as u8, 255]);
            }
        }
        DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        }
    }

    /// The worst channel difference between two readbacks of the same size.
    fn worst_difference(a: &[u8], b: &[u8]) -> i32 {
        assert_eq!(a.len(), b.len(), "the two readbacks are different sizes");
        a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).abs()).max().unwrap_or(0)
    }

    /// The whole point of the version: an image cut into tiles must draw the
    /// same picture as the same image in one texture.
    ///
    /// Compared through the frame buffer rather than by reasoning about
    /// coordinates, because a tile misplaced by its own width still satisfies
    /// every arithmetic invariant the geometry tests check.
    #[test]
    fn a_tiled_image_draws_the_same_picture_as_a_whole_one() {
        let image = test_picture(64, 48);
        let view = View::new((64, 48), (128, 96), 1.0);

        let Some(whole) = draw_tiled(&image, 4096, &view, Orientation::Normal, (128, 96)) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        // A limit that cuts this picture into a grid of tiles in both axes.
        let tiled = draw_tiled(&image, 20, &view, Orientation::Normal, (128, 96)).expect("the adapter answered once already");

        let worst = worst_difference(&whole, &tiled);
        assert!(worst <= 1, "the tiled picture differs from the whole one by {worst}/255");
    }

    /// The same, magnified — where a seam actually shows.
    ///
    /// A picture blown up past one texel per pixel is the case the overlap
    /// exists for: without it the sampler clamps at each tile's edge and the
    /// join reads as a flat step. Measured at eight values out of 255 before
    /// the overlap was added, which is why the tolerance here is 1.
    #[test]
    fn a_magnified_tiled_image_has_no_seam() {
        let image = test_picture(16, 16);
        let mut view = View::new((16, 16), (256, 256), 1.0);
        view.zoom_at(24.0, (128.0, 128.0));

        let Some(whole) = draw_tiled(&image, 4096, &view, Orientation::Normal, (256, 256)) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let tiled = draw_tiled(&image, 6, &view, Orientation::Normal, (256, 256)).expect("the adapter answered once already");

        let worst = worst_difference(&whole, &tiled);
        assert!(worst <= 1, "a seam shows at magnification: {worst}/255 away from the whole picture");
    }

    /// Orientation is applied to the whole rectangle, so tiles have to follow
    /// it. A rotated tiled image must match the rotated whole one — the case
    /// where placing a tile in image space rather than screen space gives a
    /// picture assembled inside out.
    #[test]
    fn tiles_follow_the_exif_orientation() {
        let image = test_picture(48, 32);
        // Rotation swaps the axes, which is what the view is told about.
        let view = View::new((32, 48), (128, 128), 1.0);

        for orientation in [
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::FlipHorizontal,
            Orientation::Transpose,
        ] {
            let Some(whole) = draw_tiled(&image, 4096, &view, orientation, (128, 128)) else {
                eprintln!("skipping: no graphics adapter here");
                return;
            };
            let tiled = draw_tiled(&image, 20, &view, orientation, (128, 128)).expect("the adapter answered once already");

            let worst = worst_difference(&whole, &tiled);
            assert!(worst <= 1, "{orientation:?}: the tiled picture differs by {worst}/255");
        }
    }

    /// Panned and zoomed, the tiles must still land where the whole picture
    /// would: `for_tile` derives each quad from the placed rectangle, so an
    /// error in that derivation shows only once the rectangle is off-centre.
    #[test]
    fn tiles_follow_zoom_and_pan() {
        let image = test_picture(40, 40);
        let mut view = View::new((40, 40), (160, 160), 1.0);
        view.zoom_at(8.0, (40.0, 120.0));
        view.pan((17.0, -23.0));

        let Some(whole) = draw_tiled(&image, 4096, &view, Orientation::Normal, (160, 160)) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let tiled = draw_tiled(&image, 14, &view, Orientation::Normal, (160, 160)).expect("the adapter answered once already");

        let worst = worst_difference(&whole, &tiled);
        assert!(worst <= 1, "under zoom and pan the tiled picture differs by {worst}/255");
    }

    /// `to_screen` must be the exact inverse of what the shader does, checked
    /// against the shader's own matrix rather than against a second table of
    /// the eight cases — a table would just be the same guess written twice.
    ///
    /// The transpose passes this for the four orientations that keep the axes
    /// and fails it for the four that exchange them, which is how the bug it
    /// guards against reached a rendered frame.
    #[test]
    fn a_tile_offset_travels_to_the_screen_and_back_unchanged() {
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
            for offset in [[1.0, 0.0], [0.0, 1.0], [-0.5, 0.25], [0.3, -0.7]] {
                let (on_screen, _, _) = to_screen(orientation, offset, 1.0, 1.0);

                // Put the screen offset through the shader's own arithmetic:
                // `m * (corner.x, -corner.y)`, WGSL reading `m` column-major.
                let v = [on_screen[0], -on_screen[1]];
                let back = [m[0][0] * v[0] + m[1][0] * v[1], m[0][1] * v[0] + m[1][1] * v[1]];

                assert!(
                    (back[0] - offset[0]).abs() < 1e-5 && (back[1] - offset[1]).abs() < 1e-5,
                    "{orientation:?}: {offset:?} went to the screen as {on_screen:?} and came back as {back:?}",
                );
            }
        }
    }

    /// `to_screen` undoes the orientation with a transpose, which is only the
    /// inverse if every one of these matrices is orthogonal. Four of them are
    /// reflections, with determinant -1 — that does not stop the transpose
    /// being the inverse, but it is exactly the sort of thing that gets
    /// assumed rather than checked, so it is checked.
    #[test]
    fn every_orientation_matrix_is_its_own_inverse_transposed() {
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
            // `m` times its transpose must be the identity.
            for row in 0..2 {
                for column in 0..2 {
                    let product = m[0][row] * m[0][column] + m[1][row] * m[1][column];
                    let expected = if row == column { 1.0 } else { 0.0 };
                    assert!(
                        (product - expected).abs() < 1e-6,
                        "{orientation:?} is not orthogonal: entry ({row},{column}) of M*Mt is {product}",
                    );
                }
            }
        }
    }

    /// A rotation exchanges the axes of the span as well as its direction: a
    /// tile that is wide in the image is tall on screen.
    #[test]
    fn a_quarter_turn_exchanges_a_tiles_screen_size() {
        let (_, width, height) = to_screen(Orientation::Rotate90, [0.0, 0.0], 0.25, 1.0);
        assert_eq!((width, height), (1.0, 0.25));

        let (_, width, height) = to_screen(Orientation::Rotate180, [0.0, 0.0], 0.25, 1.0);
        assert_eq!((width, height), (0.25, 1.0));
    }

    /// A whole-image placement samples all of its texture; only a tile insets.
    #[test]
    fn an_untiled_placement_samples_the_whole_texture() {
        let view = View::new((100, 100), (100, 100), 1.0);
        let placement = Placement::new(&view, (100, 100), Orientation::Normal);
        assert_eq!(placement.uv_scale, [1.0, 1.0]);
        assert_eq!(placement.uv_offset, [0.0, 0.0]);
    }

    /// The single-tile case must come out of `for_tile` unchanged: it is the
    /// path every ordinary photograph takes, and it has to cost nothing.
    #[test]
    fn a_single_tile_placement_is_the_whole_image_placement() {
        let view = View::new((4000, 3000), (1000, 800), 1.0);
        let whole = Placement::new(&view, (1000, 800), Orientation::Normal);
        let grid = Grid::new((4000, 3000), 16384);
        let tile = grid.tiles()[0];

        let placed = whole.for_tile(Orientation::Normal, tile.span((4000, 3000)), tile.inner_uv());
        assert_eq!(placed.half_size, whole.half_size);
        assert_eq!(placed.centre, whole.centre);
        assert_eq!(placed.uv_scale, [1.0, 1.0]);
        assert_eq!(placed.uv_offset, [0.0, 0.0]);
    }

    /// Tiles must partition the image rectangle: their quads have to add up
    /// to the whole one, edge to edge, with no gap and no overlap.
    #[test]
    fn the_tile_quads_tile_the_image_rectangle() {
        let view = View::new((1000, 600), (1000, 600), 1.0);
        let whole = Placement::new(&view, (1000, 600), Orientation::Normal);
        let grid = Grid::new((1000, 600), 256);

        // The left and right edges of one row of tiles, in clip space.
        let mut spans: Vec<(f32, f32)> = grid
            .tiles()
            .iter()
            .filter(|tile| tile.y == 0)
            .map(|tile| {
                let placed = whole.for_tile(Orientation::Normal, tile.span((1000, 600)), tile.inner_uv());
                (placed.centre[0] - placed.half_size[0], placed.centre[0] + placed.half_size[0])
            })
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(spans.len() > 1, "this limit was meant to need several tiles");

        assert!(
            (spans[0].0 - (whole.centre[0] - whole.half_size[0])).abs() < 1e-5,
            "the first tile does not start at the image's left edge"
        );
        for pair in spans.windows(2) {
            assert!((pair[0].1 - pair[1].0).abs() < 1e-5, "a gap or an overlap between tiles: {pair:?}");
        }
        let last = spans.last().expect("at least one tile");
        assert!(
            (last.1 - (whole.centre[0] + whole.half_size[0])).abs() < 1e-5,
            "the last tile does not reach the image's right edge"
        );
    }

    /// The guard `set_image` leans on: whatever limit it is handed, no texture
    /// it goes on to create may exceed what the device accepts.
    ///
    /// This is the check that catches a cut made to the wrong limit — the
    /// mutation that replaces `self.tile_limit` with something larger. Every
    /// other tiling test builds its own `Grid`, so none of them would notice.
    #[test]
    fn a_plan_never_exceeds_the_devices_limit() {
        // Cut to the device's own limit: a plan, and every tile within it.
        for size in [(4000, 3000), (30000, 20000), (16385, 100)] {
            let grid = plan_tiles(size, 16384, 16384).expect("the device's own limit always has a plan");
            for tile in grid.tiles() {
                assert!(
                    tile.padded_width <= 16384 && tile.padded_height <= 16384,
                    "{size:?}: {tile:?} exceeds the limit"
                );
            }
        }

        // Cut to a limit larger than the device allows: refused rather than
        // handed on. The viewer shows nothing, which beats a panic.
        assert!(plan_tiles((30000, 20000), u32::MAX, 16384).is_none());
        assert!(plan_tiles((20000, 100), 20000, 16384).is_none());

        // Small enough for the device either way: still a plan.
        assert!(plan_tiles((800, 600), u32::MAX, 16384).is_some());
    }

    /// The lever the tests need, and the guard that it can never make things
    /// worse: it may lower the limit, never raise it past the device.
    #[test]
    fn the_tile_limit_lever_never_exceeds_the_device() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else {
            eprintln!("skipping: no graphics adapter here");
            return;
        };
        let Ok((device, _queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            ..Default::default()
        })) else {
            eprintln!("skipping: no device here");
            return;
        };
        let device_limit = device.limits().max_texture_dimension_2d;
        let _guard = ENVIRONMENT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        // Unset: the device's own limit.
        unsafe { std::env::remove_var("NITID_TILE_LIMIT") };
        assert_eq!(tile_limit(&device), device_limit);

        // Set lower: taken.
        unsafe { std::env::set_var("NITID_TILE_LIMIT", "64") };
        assert_eq!(tile_limit(&device), 64);

        // Higher than the device allows: clamped, because a texture past the
        // device's limit is the crash being prevented here.
        unsafe { std::env::set_var("NITID_TILE_LIMIT", (u64::from(device_limit) * 4).to_string()) };
        assert_eq!(tile_limit(&device), device_limit);

        // Nonsense: ignored rather than obeyed.
        unsafe { std::env::set_var("NITID_TILE_LIMIT", "not a number") };
        assert_eq!(tile_limit(&device), device_limit);
        unsafe { std::env::set_var("NITID_TILE_LIMIT", "0") };
        assert_eq!(tile_limit(&device), device_limit);

        unsafe { std::env::remove_var("NITID_TILE_LIMIT") };
    }
}
