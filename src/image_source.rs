//! Decoding an image file into pixels the renderer can upload.
//!
//! Every decoder in this module is pure Rust: a malformed file costs a panic or
//! an error, never code execution. That includes the two formats the sandbox
//! of `docs/adr/0002-sandbox-c-decoders.md` was built for — HEIC decodes in
//! Rust (ADR 0007) and AVIF decodes through `rav1d`, dav1d translated to Rust
//! (ADR 0008). The boundary stands unused, and `Format::needs_sandbox` is
//! still the one word that would put a decoder behind it.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};
use moxcms::ColorProfile;

use crate::color;
use crate::format::Format;
use crate::vector::VectorImage;

/// Extensions this build can open, lowercase and without the dot.
///
/// Derived from the formats rather than written out, so a new decoder reaches
/// the folder listing and the file associations without a second list to keep
/// in step.
pub fn supported_extensions() -> Vec<&'static str> {
    Format::ALL.iter().flat_map(|format| format.extensions().iter().copied()).collect()
}

/// How wide one sample of a decoded image is.
///
/// Most formats deliver eight bits per channel and always will. Ten- and
/// twelve-bit sources — HDR AVIF and HEIC — arrive as sixteen, scaled to the
/// full range, because that is the texture width the GPU offers next to
/// eight; carrying "ten" through the pipeline would add a case every stage
/// must handle and no stage wants.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Depth {
    #[default]
    Eight,
    /// Samples are `u16` in native byte order, two bytes each in `pixels`.
    Sixteen,
}

impl Depth {
    /// Bytes per sample.
    pub fn bytes(self) -> u32 {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
}

/// A decoded image: tightly packed RGBA rows, ready for a GPU upload.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` samples, row-major, no padding. A sample is one
    /// byte or a native-endian `u16` pair, as `depth` says.
    pub pixels: Vec<u8>,
    /// How wide each sample is.
    pub depth: Depth,
}

impl DecodedImage {
    /// Bytes per row as the GPU sees them.
    pub fn bytes_per_row(&self) -> u32 {
        self.width * 4 * self.depth.bytes()
    }
}

/// How the encoded pixels must be rotated and flipped to appear upright.
///
/// The values follow EXIF tag 0x0112. The renderer applies this as a transform
/// on texture coordinates, so the decoded pixels are never rewritten in memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Orientation {
    #[default]
    Normal,
    FlipHorizontal,
    Rotate180,
    FlipVertical,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

impl Orientation {
    pub fn from_exif(value: u16) -> Self {
        match value {
            2 => Self::FlipHorizontal,
            3 => Self::Rotate180,
            4 => Self::FlipVertical,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Normal,
        }
    }

    /// The EXIF number for this orientation.
    ///
    /// The inverse of `from_exif`, for the decoder process: the orientation
    /// crosses the boundary as the number every format already speaks rather
    /// than as a private encoding the two halves would have to agree on.
    pub fn to_exif(self) -> u8 {
        match self {
            Self::Normal => 1,
            Self::FlipHorizontal => 2,
            Self::Rotate180 => 3,
            Self::FlipVertical => 4,
            Self::Transpose => 5,
            Self::Rotate90 => 6,
            Self::Transverse => 7,
            Self::Rotate270 => 8,
        }
    }

    /// Whether the transform exchanges width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270)
    }

    /// This orientation as the 2x2 matrix the renderer uses.
    ///
    /// The renderer maps quad corners to texture space, so this is the
    /// *inverse* of what the picture visibly does. Kept here rather than only
    /// in `gpu.rs` because composing two orientations is matrix multiplication
    /// and nothing else — a hand-written table of "what follows what" is eight
    /// chances to get a sign wrong, which is exactly how the tile orientation
    /// went wrong in v0.15.0.
    pub const fn matrix(self) -> [[i8; 2]; 2] {
        match self {
            Self::Normal => [[1, 0], [0, 1]],
            Self::FlipHorizontal => [[-1, 0], [0, 1]],
            Self::Rotate180 => [[-1, 0], [0, -1]],
            Self::FlipVertical => [[1, 0], [0, -1]],
            Self::Transpose => [[0, 1], [1, 0]],
            Self::Rotate90 => [[0, 1], [-1, 0]],
            Self::Transverse => [[0, -1], [-1, 0]],
            Self::Rotate270 => [[0, -1], [1, 0]],
        }
    }

    /// The orientation whose matrix is `matrix`, if it is one of the eight.
    fn from_matrix(matrix: [[i8; 2]; 2]) -> Option<Self> {
        [
            Self::Normal,
            Self::FlipHorizontal,
            Self::Rotate180,
            Self::FlipVertical,
            Self::Transpose,
            Self::Rotate90,
            Self::Transverse,
            Self::Rotate270,
        ]
        .into_iter()
        .find(|candidate| candidate.matrix() == matrix)
    }

    /// This orientation followed by `then`, as one orientation.
    ///
    /// Composition is matrix multiplication and nothing else. The order is
    /// `self * then`, derived rather than guessed: tracking where a texel
    /// lands on screen through a quarter turn picks this order and not the
    /// other, and the two differ precisely on the mirrored orientations —
    /// where a plausible-looking table would be wrong and no ordinary
    /// photograph would reveal it.
    pub fn then(self, then: Self) -> Self {
        let (a, b) = (self.matrix(), then.matrix());
        let product = [
            [a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]],
            [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]],
        ];
        // The eight orientations are a group under multiplication, so the
        // product is always one of them; the fallback is unreachable and is
        // held down by a test rather than left to trust.
        Self::from_matrix(product).unwrap_or(self)
    }

    /// Turn the picture a quarter turn, as the person looking at it sees it.
    ///
    /// This is a viewing transform: the file is not touched, and stepping to
    /// another image starts from whatever that file asks for. Rotating the
    /// file itself is a different feature and a later version.
    pub fn turned(self, clockwise: bool) -> Self {
        self.then(if clockwise { Self::Rotate90 } else { Self::Rotate270 })
    }
}

/// Whether an image is the real thing or the placeholder shown while it loads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fidelity {
    /// The camera's embedded thumbnail: right shape, wrong detail.
    Thumbnail,
    /// The image as the file actually stores it.
    Full,
}

/// A file loaded and decoded, with the orientation its metadata asks for.
#[derive(Clone)]
pub struct LoadedImage {
    pub image: DecodedImage,
    pub orientation: Orientation,
    pub fidelity: Fidelity,
    /// The format the bytes turned out to be, decided by content rather than
    /// by the file's extension. Shown in the status line.
    pub format: Format,
    /// What the file says about itself: the camera, the exposure, the place.
    ///
    /// Read on this side of the sandbox from the same bytes, never carried
    /// across the protocol — the decoder is the untrusted half, and what a
    /// file *says* is settled before anything is handed to it. Reading costs
    /// 0.03 to 1.7 ms measured, so it happens on every open rather than being
    /// deferred to the first time the panel is asked for.
    pub metadata: crate::metadata::Metadata,
    /// The ICC profile the file carries, if any.
    ///
    /// `None` means untagged, which by convention means sRGB — the assumption
    /// every viewer makes and the one that is nearly always right.
    pub profile: Option<ColorProfile>,
    /// The document this image was drawn from, for formats that describe
    /// shapes rather than pixels.
    ///
    /// `None` for every raster format, where the decoded pixels *are* the
    /// image. When present, the viewer can draw the picture again at another
    /// size instead of scaling what it already has — which is the whole reason
    /// to open a vector format at all.
    pub vector: Option<VectorImage>,
    /// Every frame of an animated image, when there is more than one.
    ///
    /// `image` above is the first of these frames, so a caller that ignores
    /// this field shows exactly what earlier versions showed. Behind an `Arc`
    /// because the loader clones `LoadedImage` out of its prefetch cache, and
    /// an animation is the one part worth not copying.
    pub animation: Option<std::sync::Arc<crate::animation::Animation>>,
}

impl LoadedImage {
    /// Size after the orientation transform — what the viewer lays out against.
    pub fn display_size(&self) -> (u32, u32) {
        if self.orientation.swaps_axes() {
            (self.image.height, self.image.width)
        } else {
            (self.image.width, self.image.height)
        }
    }
}

/// Whether the path carries an extension this build can decode.
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| supported_extensions().contains(&ext.as_str()))
}

/// Decode an image already in memory.
///
/// The format is detected from the content rather than the extension: a `.png`
/// that is really a JPEG is common enough that trusting the name would produce
/// a spurious error on a file the user can plainly see elsewhere.
pub fn decode(bytes: &[u8]) -> Result<LoadedImage> {
    decode_with(bytes, Confinement::Sandboxed)
}

/// Decode without handing anything to a sandbox, whatever the format asks for.
///
/// This is what the decoder process itself calls. Without it a format needing
/// the sandbox would launch a sandbox from inside the sandbox, for ever.
pub fn decode_here(bytes: &[u8]) -> Result<LoadedImage> {
    decode_with(bytes, Confinement::InThisProcess)
}

/// Whether a decoder that asks for a sandbox is given one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Confinement {
    Sandboxed,
    InThisProcess,
}

fn decode_with(bytes: &[u8], confinement: Confinement) -> Result<LoadedImage> {
    if bytes.is_empty() {
        bail!("the file is empty");
    }

    let Some(format) = Format::detect(bytes) else {
        bail!("the format is not one nitid can open");
    };

    // A format decoded by a C library is decoded somewhere it can do no harm.
    // The pixels come back over a pipe; everything else about the file — its
    // profile, its orientation — is read here, in Rust, from the same bytes.
    if format.needs_sandbox() && confinement == Confinement::Sandboxed {
        // Everything the file says about itself comes back with the pixels.
        // Reading the profile and the orientation again here would work for a
        // format that states them in its container, and lose them for one
        // that states them in the bitstream — which is the case for AVIF.
        // Neither the format nor the metadata is part of that round trip:
        // both are already known here from the same bytes, so they are set on
        // this side rather than added to the protocol the sandboxed process
        // speaks — the decoder is the untrusted half.
        let mut loaded = crate::sandbox::decode(bytes, format).with_context(|| format!("decoding the {}", format.name()))?;
        loaded.metadata = crate::metadata::read(bytes);
        return Ok(loaded);
    }

    let malformed = || format!("the {} data is malformed", format.name());

    // The formats that animate are asked for their frames first. `Some` here
    // covers even a single-frame GIF — the frame is already decoded, and
    // decoding the same bytes twice to call it a still would be waste. A
    // still PNG or WebP comes back `None` and takes the ordinary path.
    if matches!(format, Format::Gif | Format::Png | Format::WebP)
        && let Some(animation) = crate::animation::decode(format, bytes)
    {
        let image = animation.frames[0].image.clone();
        return Ok(LoadedImage {
            image,
            // None of the three formats states an EXIF orientation in
            // practice, and their decoders deliver the canvas as shown.
            orientation: Orientation::Normal,
            metadata: crate::metadata::read(bytes),
            fidelity: Fidelity::Full,
            format,
            profile: color::profile_from(bytes),
            vector: None,
            animation: (animation.frames.len() > 1).then(|| std::sync::Arc::new(animation)),
        });
    }

    // A vector image is parsed rather than decoded, and the result is kept:
    // the pixels here are only the first rasterisation, at the size the
    // document declares, and the viewer draws it again whenever the size on
    // screen changes.
    if format.is_vector() {
        let vector = VectorImage::parse(bytes).with_context(malformed)?;
        let (width, height) = vector.intrinsic_size();
        let image = vector.rasterise(width, height).with_context(malformed)?;

        return Ok(LoadedImage {
            image,
            orientation: Orientation::Normal,
            fidelity: Fidelity::Full,
            format,
            metadata: crate::metadata::read(bytes),
            profile: None,
            vector: Some(vector),
            animation: None,
        });
    }

    // JPEG XL comes back with its profile attached: one pass over the file
    // yields both, where reading the profile separately — as the other formats
    // do — would mean decoding the image twice.
    // AVIF states its rotation in the container rather than in EXIF, and the
    // decoder reads it while walking the boxes; it overrides the EXIF tag
    // below because a file carrying both means the same turn twice.
    let mut avif_orientation = None;

    let (image, profile) = match format {
        Format::JpegXl => decode_jxl(bytes).with_context(malformed)?,
        // AVIF states its colour inside the AV1 bitstream, which the decoder
        // reads on the way past; asking a second parser for it afterwards
        // would mean walking the same bytes again.
        Format::Avif => {
            let decoded = crate::avif::decode(bytes).with_context(malformed)?;
            avif_orientation = decoded.orientation;
            (decoded.image, decoded.profile)
        }
        Format::Jpeg => (decode_jpeg(bytes).with_context(malformed)?, color::profile_from(bytes)),
        Format::WebP => (decode_webp(bytes).with_context(malformed)?, color::profile_from(bytes)),
        Format::Heic => decode_heic_with_colour(bytes).with_context(malformed)?,
        // PNG, GIF, BMP and TIFF share one pure-Rust decoder.
        _ => (decode_via_image_crate(bytes).with_context(malformed)?, color::profile_from(bytes)),
    };

    // A format that turns its own pixels the right way up has already done so;
    // applying the EXIF tag as well would rotate the image twice.
    let orientation = if format.orients_itself() {
        Orientation::Normal
    } else {
        avif_orientation.unwrap_or_else(|| read_orientation(bytes))
    };

    Ok(LoadedImage {
        image,
        orientation,
        fidelity: Fidelity::Full,
        format,
        metadata: crate::metadata::read(bytes),
        profile,
        // A raster format is its pixels; there is nothing to redraw from.
        vector: None,
        animation: None,
    })
}

/// Decode the thumbnail a camera embedded in the file, if there is one.
///
/// This is the first frame the viewer shows. A JPEG thumbnail is a few
/// kilobytes against tens of megabytes for the full image, so it decodes in
/// single-digit milliseconds — the difference between a window that appears
/// with a picture in it and one that appears empty and fills in later.
///
/// Returns `None` whenever there is no usable thumbnail, which is not an
/// error: most PNGs and every screenshot lack one, and the full decode covers
/// that case on its own.
pub fn decode_thumbnail(bytes: &[u8]) -> Option<LoadedImage> {
    let format = Format::detect(bytes)?;

    // HEIC keeps its thumbnail as a second coded image in the container rather
    // than in EXIF, so it is reached a different way.
    if format == Format::Heic {
        return decode_heic_thumbnail(bytes);
    }

    let exif = read_exif(bytes)?;

    // The thumbnail lives at an offset inside the EXIF block, described by two
    // tags in the thumbnail IFD.
    let offset = exif
        .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
        .and_then(|field| field.value.get_uint(0))? as usize;
    let length = exif
        .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
        .and_then(|field| field.value.get_uint(0))? as usize;

    let buffer = exif.buf();
    let end = offset.checked_add(length)?;
    if length == 0 || end > buffer.len() {
        // A truncated or lying descriptor: fall through to the full decode
        // rather than handing the decoder a slice of something else.
        return None;
    }

    let thumbnail = decode_jpeg(&buffer[offset..end]).ok()?;

    Some(LoadedImage {
        image: thumbnail,
        // The same file, so the same metadata: the quick frame is what the
        // panel describes until the full image replaces it.
        metadata: crate::metadata::read(bytes),
        // The orientation tag lives in the primary IFD and applies to both the
        // full image and its thumbnail, so the quick frame is not shown
        // sideways for the moment before the real one replaces it.
        orientation: orientation_from(&exif),
        fidelity: Fidelity::Thumbnail,
        format,
        // The profile describes the file, so it covers the thumbnail too: the
        // quick frame and the image replacing it are the same colour.
        profile: color::profile_from(bytes),
        // Thumbnails come from EXIF, which only raster formats carry.
        vector: None,
        animation: None,
    })
}

/// Decode the thumbnail a camera stored inside a HEIC.
///
/// A HEIC carries no EXIF thumbnail. What it has instead is a second, small
/// picture in the same container, tied to the full one by a `thmb` reference —
/// which is what a phone writes, and what the shell shows in a folder.
///
/// `heif-oxide` decodes the primary item and offers no way to ask for another,
/// so the file is handed back to it with the `pitm` box rewritten to name the
/// thumbnail. That is a two-byte edit to a copy of the header; nothing else
/// about the file changes, and the decoder does the reading as it always does.
/// The alternative — assembling a standalone HEIC around the thumbnail's
/// coded data — would mean rebuilding `iloc`, `iinf`, `ipco` and `ipma` by
/// hand, which is a second container writer to keep correct for no gain.
///
/// The difference this makes is the whole point: on a 12-megapixel photograph
/// the thumbnail decodes in about 5 ms against 470 ms for the full image.
fn decode_heic_thumbnail(bytes: &[u8]) -> Option<LoadedImage> {
    let primary = crate::isobmff::primary_item(bytes)?;
    let thumbnail = crate::isobmff::thumbnail_of(bytes, primary.id)?;

    // Only the header is copied: `pitm` sits near the front, and the coded
    // images — the bulk of the file — are left where they are.
    let mut rewritten = bytes.to_vec();
    let identifier = rewritten.get_mut(primary.offset..primary.offset + primary.width)?;
    match primary.width {
        2 => identifier.copy_from_slice(&u16::try_from(thumbnail).ok()?.to_be_bytes()),
        _ => identifier.copy_from_slice(&thumbnail.to_be_bytes()),
    }

    let image = decode_heic(&rewritten).ok()?;

    Some(LoadedImage {
        image,
        metadata: crate::metadata::read(bytes),
        // HEIC states its rotation in the container and the decoder applies
        // it — to the thumbnail as much as to the full image, since both are
        // items in the same file. See `Format::orients_itself`.
        orientation: Orientation::Normal,
        fidelity: Fidelity::Thumbnail,
        // Only reached through the HEIC branch above.
        format: Format::Heic,
        // The pixels arrive as sRGB, the same as the full decode: attaching a
        // profile would convert them twice (ADR 0007).
        profile: None,
        vector: None,
        animation: None,
    })
}

/// JPEG goes through `zune-jpeg` rather than the `image` crate: it is the
/// faster decoder, and JPEG is the format the viewer opens most.
fn decode_jpeg(bytes: &[u8]) -> Result<DecodedImage> {
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(Cursor::new(bytes), options);
    let pixels = decoder.decode().context("the JPEG data is malformed")?;
    let info = decoder.info().context("the JPEG carries no frame header")?;

    let width = u32::from(info.width);
    let height = u32::from(info.height);
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        bail!("the JPEG decoded to {} bytes but {}x{} RGBA needs {expected}", pixels.len(), width, height);
    }

    Ok(DecodedImage {
        width,
        height,
        pixels,
        depth: Depth::Eight,
    })
}

fn decode_via_image_crate(bytes: &[u8]) -> Result<DecodedImage> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("the format could not be recognised")?;
    if reader.format().is_none() {
        bail!("the format is not one nitid can open");
    }

    let decoded = reader.decode().context("the image data is malformed")?;
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();

    Ok(DecodedImage {
        width,
        height,
        pixels: rgba.into_raw(),
        depth: Depth::Eight,
    })
}

/// WebP goes through `image-webp` directly rather than through the `image`
/// crate wrapping it, because only the crate itself exposes the ICC profile —
/// and an untagged wide-gamut WebP shown as sRGB is exactly the silent colour
/// error this viewer exists to avoid.
fn decode_webp(bytes: &[u8]) -> Result<DecodedImage> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).context("the WebP header is unreadable")?;
    let (width, height) = decoder.dimensions();

    let expected = pixel_count(width, height, Depth::Eight)?;
    let mut pixels = vec![0u8; expected];

    if decoder.has_alpha() {
        decoder.read_image(&mut pixels).context("the WebP pixels are unreadable")?;
    } else {
        // Without alpha the decoder writes three bytes per pixel, so it reads
        // into its own buffer and the opaque alpha is filled in here.
        let mut rgb = vec![0u8; width as usize * height as usize * 3];
        decoder.read_image(&mut rgb).context("the WebP pixels are unreadable")?;
        pixels = rgb_to_rgba8(&rgb, expected);
    }

    Ok(DecodedImage {
        width,
        height,
        pixels,
        depth: Depth::Eight,
    })
}

/// Decode a JPEG XL, returning the pixels together with the profile the file
/// carries.
///
/// Decode a HEIC, and say what colour its pixels are in.
///
/// Nearly every HEIC describes its colour with CICP code points in a `colr`
/// box, and `heif-oxide` reads those: a Display P3 photograph is converted to
/// sRGB on the way out, which is the limitation ADR 0007 records.
///
/// A file that carries an **ICC profile instead** is a different matter, and
/// a worse one. The decoder notices the profile but does not read it, so it
/// falls back to a default set of matrix coefficients — and that default is
/// wrong often enough to be visible: a flat green that libheif reads as
/// (10, 200, 90) comes back as (0, 185, 85).
///
/// So for such a file the `colr` box is rewritten, in a copy, to state the
/// coefficients the pixels were actually coded with. The decoder then produces
/// the same pixels libheif does, and the ICC profile is handed on to the
/// shader — which is how every other format in the viewer is treated, and
/// better than what the CICP path manages.
fn decode_heic_with_colour(bytes: &[u8]) -> Result<(DecodedImage, Option<ColorProfile>)> {
    let Some(colour) = crate::isobmff::colour_box(bytes).filter(|colour| colour.is_icc) else {
        // The ordinary case: the file states CICP codes, the decoder reads
        // them and resolves the colour itself. Nothing to attach — see
        // `color::raw_profile`.
        return Ok((decode_heic(bytes)?, color::profile_from(bytes)));
    };

    let mut rewritten = bytes.to_vec();
    // `colr` is a kind tag followed by its payload. An ICC box is far larger
    // than the seven bytes an `nclx` one needs, so the codes are written over
    // the front of the profile and the rest of the box is left as it lies —
    // the decoder reads the codes and stops.
    let Some(body) = rewritten.get_mut(colour.kind_offset..colour.kind_offset + 11) else {
        return Ok((decode_heic(bytes)?, color::profile_from(bytes)));
    };

    body[..4].copy_from_slice(b"nclx");
    // BT.709 primaries and the sRGB transfer, so the decoder does not convert
    // the primaries at all — the profile below describes them instead. The
    // matrix is what actually matters here: BT.601 is what these files are
    // coded with, and assuming BT.709 is the error being corrected.
    body[4..6].copy_from_slice(&1u16.to_be_bytes());
    body[6..8].copy_from_slice(&13u16.to_be_bytes());
    body[8..10].copy_from_slice(&6u16.to_be_bytes());
    // Full range, as a still image is.
    body[10] = 0x80;

    let image = decode_heic(&rewritten)?;
    Ok((image, color::profile_from(bytes)))
}

/// Decode a HEIC — the format a modern iPhone photographs in.
///
/// Pure Rust, container and HEVC alike, which is the reason this format does
/// not go through the sandbox built in v0.6.0: there is no C library here to
/// confine. See `docs/adr/0007-heic-decodes-in-rust.md`.
///
/// The decoder delivers sRGB. A photograph tagged Display P3 has therefore
/// already been converted by the time it arrives, which is why no profile is
/// attached to it: the pixels are in the space the profile would convert them
/// *to*. That costs a wide-gamut photograph the part of the display's gamut
/// sRGB cannot reach, and it is the one place in the viewer where colour is
/// resolved on the way in rather than in the shader — recorded as a
/// limitation, not left to be discovered.
fn decode_heic(bytes: &[u8]) -> Result<DecodedImage> {
    // The container is parsed by the same crate that decodes the pixels, so a
    // file that opens elsewhere and not here fails as one error rather than
    // half-succeeding.
    let image = heif_oxide::decode_bytes(bytes).map_err(|error| anyhow::anyhow!("{error}"))?;

    let width = image.width;
    let height = image.height;

    // A 10- or 12-bit source comes back as 16-bit samples, already scaled to
    // the full range, and is kept that wide: the renderer takes sixteen-bit
    // textures now, and narrowing here was the debt this stage repays.
    let (pixels, depth) = match &image.pixels {
        heif_oxide::Pixels::Rgb8(_) | heif_oxide::Pixels::Rgba8(_) => (image.to_rgba8(), Depth::Eight),
        heif_oxide::Pixels::Rgb16(samples) => (rgb16_to_rgba_bytes(samples, false), Depth::Sixteen),
        heif_oxide::Pixels::Rgba16(samples) => (rgb16_to_rgba_bytes(samples, true), Depth::Sixteen),
    };

    let expected = pixel_count(width, height, depth)?;
    if pixels.len() != expected {
        bail!("the HEIC decoded to {} bytes but {width}x{height} RGBA needs {expected}", pixels.len());
    }

    Ok(DecodedImage { width, height, pixels, depth })
}

/// Interleave 16-bit samples into RGBA bytes, native-endian.
///
/// `has_alpha` says whether the samples come in fours already or in threes
/// needing an opaque alpha appended.
fn rgb16_to_rgba_bytes(samples: &[u16], has_alpha: bool) -> Vec<u8> {
    if has_alpha {
        return bytemuck::cast_slice(samples).to_vec();
    }
    let mut out = Vec::with_capacity(samples.len() / 3 * 8);
    for rgb in samples.as_chunks::<3>().0 {
        for sample in rgb {
            out.extend_from_slice(&sample.to_ne_bytes());
        }
        out.extend_from_slice(&u16::MAX.to_ne_bytes());
    }
    out
}

/// The two come back together because `jxl-oxide` reaches both in the same
/// pass: asking for the profile separately would mean parsing the codestream a
/// second time, and JXL is not a format where that is cheap.
///
/// The renderer is given the first keyframe. An animated JXL therefore shows
/// its opening frame, which is what every other still viewer does and what the
/// animation stage (v0.9.0) will replace.
fn decode_jxl(bytes: &[u8]) -> Result<(DecodedImage, Option<ColorProfile>)> {
    let image = jxl_oxide::JxlImage::builder()
        .read(bytes)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("the JPEG XL header is unreadable")?;

    // A profile the file states outright is preferred over the one jxl-oxide
    // synthesises from an enumerated colour space: an untagged image must stay
    // untagged, or it would be converted against the rule ADR 0005 sets out.
    let profile = image.original_icc().and_then(|raw| ColorProfile::new_from_slice(raw).ok());

    let render = image
        .render_frame(0)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("the JPEG XL pixels are unreadable")?;

    // The renderer applies the header's orientation to both the pixels and the
    // dimensions, so the two agree here — see `Format::orients_itself`.
    let mut stream = render.stream();
    let width = stream.width();
    let height = stream.height();
    let channels = stream.channels() as usize;

    let expected = pixel_count(width, height, Depth::Eight)?;
    let mut samples = vec![0u8; expected / 4 * channels];
    stream.write_to_buffer(&mut samples);

    // The stream carries as many channels as the image has: four for RGBA,
    // three for opaque colour, two for grey with alpha, one for plain grey.
    // All of them are widened to the RGBA the renderer uploads, because a
    // format is either supported or it is not — a greyscale scan refusing to
    // open would be exactly the half-support `format.rs` exists to prevent.
    let pixels = match channels {
        4 => samples,
        3 => rgb_to_rgba8(&samples, expected),
        2 => grey_to_rgba8(samples.as_chunks::<2>().0.iter().map(|pair| (pair[0], pair[1])), expected),
        1 => grey_to_rgba8(samples.iter().map(|grey| (*grey, 0xFF)), expected),
        // CMYK reaches the same stream with five channels or more. Left
        // unhandled rather than guessed at: converting it needs the profile,
        // and no such file has been seen in the wild here.
        other => bail!("a JPEG XL with {other} channels per pixel is not one this build can show"),
    };

    Ok((
        DecodedImage {
            width,
            height,
            pixels,
            depth: Depth::Eight,
        },
        profile,
    ))
}

/// Bytes needed for a `width` by `height` RGBA8 image.
///
/// Checked rather than multiplied: the dimensions come from a file that may be
/// lying, and a 32-bit overflow here would size the buffer far too small.
pub(crate) fn pixel_count(width: u32, height: u32, depth: Depth) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4 * depth.bytes() as usize))
        .filter(|bytes| *bytes > 0)
        .with_context(|| format!("{width}x{height} is not an image this build can hold"))
}

/// Widen grey samples to RGBA8, repeating the one value across all three
/// colour channels.
///
/// Greyscale images arrive this way from JPEG XL, where a scan or a black and
/// white photograph is stored with a single channel rather than three equal
/// ones.
fn grey_to_rgba8(samples: impl Iterator<Item = (u8, u8)>, expected: usize) -> Vec<u8> {
    let mut pixels = vec![0xFFu8; expected];
    for (target, (grey, alpha)) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(samples) {
        target[..3].fill(grey);
        target[3] = alpha;
    }
    pixels
}

/// Widen opaque RGB samples to the RGBA8 the renderer uploads.
pub(crate) fn rgb_to_rgba8(rgb: &[u8], expected: usize) -> Vec<u8> {
    let mut pixels = vec![0xFFu8; expected];
    for (target, source) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0) {
        target[..3].copy_from_slice(source);
    }
    pixels
}

/// Parse the EXIF block, if the file carries one.
///
/// A file without EXIF is the common case, not an error: most PNGs and every
/// screenshot lack one.
fn read_exif(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = Cursor::new(bytes);
    exif::Reader::new().read_from_container(&mut cursor).ok()
}

/// Read the EXIF orientation tag, defaulting to upright when absent.
///
/// Failures here are silent: an image shown upright is a better outcome than a
/// refusal to open.
fn read_orientation(bytes: &[u8]) -> Orientation {
    read_exif(bytes).as_ref().map(orientation_from).unwrap_or_default()
}

fn orientation_from(exif: &exif::Exif) -> Orientation {
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|field| field.value.get_uint(0))
        .map(|value| Orientation::from_exif(value as u16))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a solid-colour image so the tests do not ship binary fixtures.
    fn encode(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let buffer = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 90, 255]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut out, format)
            .expect("encoding a synthetic image");
        out.into_inner()
    }

    #[test]
    fn decodes_png_to_rgba() {
        let loaded = decode(&encode(image::ImageFormat::Png, 4, 3)).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (4, 3));
        assert_eq!(loaded.image.pixels.len(), 4 * 3 * 4);
        assert_eq!(loaded.orientation, Orientation::Normal);
    }

    #[test]
    fn decodes_jpeg_to_rgba() {
        let loaded = decode(&encode(image::ImageFormat::Jpeg, 8, 5)).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (8, 5));
        assert_eq!(loaded.image.pixels.len(), 8 * 5 * 4);
    }

    #[test]
    fn detects_format_from_content_not_extension() {
        // The bytes are a PNG; nothing in the call names an extension.
        let loaded = decode(&encode(image::ImageFormat::Png, 2, 2)).unwrap();
        assert_eq!(loaded.image.bytes_per_row(), 8);
    }

    #[test]
    fn rejects_empty_and_garbage_input() {
        assert!(decode(&[]).is_err());
        assert!(decode(b"this is not an image at all").is_err());
    }

    /// Encode a solid-colour WebP, optionally tagged with a profile.
    fn encode_webp(width: u32, height: u32, profile: Option<Vec<u8>>) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut encoder = image_webp::WebPEncoder::new(&mut out);
        if let Some(profile) = profile {
            encoder.set_icc_profile(profile);
        }
        let pixels: Vec<u8> = (0..width * height).flat_map(|_| [10u8, 200, 90]).collect();
        encoder
            .encode(&pixels, width, height, image_webp::ColorType::Rgb8)
            .expect("encoding a synthetic WebP");
        out.into_inner()
    }

    #[test]
    fn decodes_webp_to_rgba() {
        let loaded = decode(&encode_webp(6, 4, None)).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (6, 4));
        assert_eq!(loaded.image.pixels.len(), 6 * 4 * 4);
    }

    /// A WebP without alpha decodes three bytes per pixel; the fourth has to be
    /// filled in, or the image is uploaded fully transparent.
    #[test]
    fn an_opaque_webp_comes_back_opaque() {
        let loaded = decode(&encode_webp(3, 3, None)).unwrap();
        for pixel in loaded.image.pixels.as_chunks::<4>().0.iter() {
            assert_eq!(pixel[3], 0xFF, "an opaque WebP decoded to a transparent pixel");
        }
    }

    /// The colour promise applies to every format, not just the two the viewer
    /// started with: a tagged WebP shown as sRGB is wrong on a wide display.
    #[test]
    fn a_tagged_webp_carries_its_profile_through() {
        let profile = moxcms::ColorProfile::new_display_p3().encode().expect("encoding a P3 profile");
        let loaded = decode(&encode_webp(4, 4, Some(profile))).unwrap();
        assert!(loaded.profile.is_some(), "the WebP profile was dropped");
    }

    #[test]
    fn an_untagged_webp_has_no_profile() {
        assert!(decode(&encode_webp(4, 4, None)).unwrap().profile.is_none());
    }

    /// Encode a solid-colour JPEG XL, optionally tagged with a profile.
    ///
    /// The fixtures come from `jxl-encoder`, a separate implementation from the
    /// `jxl-oxide` decoder under test: a decoder fed its own encoder's output
    /// mostly proves the two agree. Lossless, so a round-trip is exact and the
    /// pixels can be asserted rather than approximated.
    ///
    /// The fixtures are deliberately small, and that is a limit worth knowing:
    /// above 256x256 a JPEG XL is split into several groups, and `jxl-oxide`
    /// rejects the multi-group files this encoder writes — while reading
    /// multi-group files from libjxl, including 2400x1600 with alpha, without
    /// complaint. Real files therefore open; the disagreement is between these
    /// two crates. Coverage at real sizes comes from files encoded by libjxl,
    /// which cannot be generated here, so it is checked by hand before a
    /// release rather than asserted in this module.
    fn encode_jxl(width: u32, height: u32, profile: Option<&[u8]>) -> Vec<u8> {
        use jxl_encoder::{ImageMetadata, LosslessConfig, PixelLayout};

        let pixels: Vec<u8> = (0..width * height).flat_map(|_| [10u8, 200, 90]).collect();
        let config = LosslessConfig::new();

        match profile {
            Some(profile) => {
                let metadata = ImageMetadata::default().with_icc_profile(profile);
                config
                    .encode_request(width, height, PixelLayout::Rgb8)
                    .with_metadata(&metadata)
                    .encode(&pixels)
                    .expect("encoding a synthetic JPEG XL")
            }
            None => config.encode(&pixels, width, height, PixelLayout::Rgb8).expect("encoding a synthetic JPEG XL"),
        }
    }

    #[test]
    fn decodes_jpeg_xl_to_rgba() {
        let loaded = decode(&encode_jxl(6, 4, None)).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (6, 4));
        assert_eq!(loaded.image.pixels.len(), 6 * 4 * 4);
    }

    /// Lossless in, lossless out: the samples the encoder was given are the
    /// samples the viewer uploads. A decoder that quietly shifted colour would
    /// fail here rather than on the author's display.
    #[test]
    fn a_lossless_jpeg_xl_round_trips_exactly() {
        let loaded = decode(&encode_jxl(3, 2, None)).unwrap();
        for pixel in loaded.image.pixels.as_chunks::<4>().0.iter() {
            assert_eq!(pixel, &[10, 200, 90, 0xFF], "a lossless JPEG XL came back with different pixels");
        }
    }

    /// The colour promise reaches JPEG XL too: a tagged file states its space
    /// and must be converted, not shown as if it were sRGB.
    #[test]
    fn a_tagged_jpeg_xl_carries_its_profile_through() {
        let profile = moxcms::ColorProfile::new_display_p3().encode().expect("encoding a P3 profile");
        let loaded = decode(&encode_jxl(4, 4, Some(&profile))).unwrap();
        assert!(loaded.profile.is_some(), "the JPEG XL profile was dropped");
    }

    /// An untagged file must stay untagged, or ADR 0005 is broken for this
    /// format: jxl-oxide can synthesise a profile from an enumerated colour
    /// space, and taking that one would convert an image that states nothing.
    #[test]
    fn an_untagged_jpeg_xl_has_no_profile() {
        assert!(decode(&encode_jxl(4, 4, None)).unwrap().profile.is_none());
    }

    /// A greyscale JPEG XL streams one sample per pixel, not three. Refusing
    /// it would be the half-support `format.rs` exists to prevent — scans and
    /// black and white photographs are stored this way.
    #[test]
    fn a_greyscale_jpeg_xl_opens_as_grey_rgba() {
        let pixels: Vec<u8> = (0..4u32 * 4).map(|i| (i * 7) as u8).collect();
        let jxl = jxl_encoder::LosslessConfig::new()
            .encode(&pixels, 4, 4, jxl_encoder::PixelLayout::Gray8)
            .expect("encoding a synthetic greyscale JPEG XL");

        let loaded = decode(&jxl).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (4, 4));
        for (pixel, grey) in loaded.image.pixels.as_chunks::<4>().0.iter().zip(&pixels) {
            assert_eq!(pixel, &[*grey, *grey, *grey, 0xFF], "a grey sample was not spread across the colour channels");
        }
    }

    const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="20">
        <rect width="30" height="20" fill="#3FA9D9"/>
    </svg>"##;

    /// An SVG opens at the size it declares, like a raster file of that size.
    #[test]
    fn decodes_svg_at_its_declared_size() {
        let loaded = decode(SVG).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (30, 20));
        assert_eq!(loaded.image.pixels.len(), 30 * 20 * 4);
    }

    /// The difference that makes a vector format worth opening: the document
    /// is kept, so the viewer can draw it again instead of scaling a raster.
    #[test]
    fn an_svg_keeps_the_document_it_was_drawn_from() {
        let loaded = decode(SVG).unwrap();
        let vector = loaded.vector.expect("an SVG should carry its document");

        let larger = vector.rasterise(300, 200).unwrap();
        assert_eq!((larger.width, larger.height), (300, 200));
    }

    /// A raster format has nothing to redraw from, and must not pretend it
    /// does — the viewer decides what to do by asking.
    #[test]
    fn a_raster_image_carries_no_document() {
        assert!(decode(&encode(image::ImageFormat::Png, 4, 4)).unwrap().vector.is_none());
        assert!(decode(&encode(image::ImageFormat::Jpeg, 4, 4)).unwrap().vector.is_none());
    }

    /// SVG states its colours in the markup and carries no ICC profile, so it
    /// must arrive untagged rather than with one invented for it.
    #[test]
    fn an_svg_has_no_colour_profile() {
        assert!(decode(SVG).unwrap().profile.is_none());
    }

    #[test]
    fn widening_grey_samples_fills_all_three_colour_channels() {
        assert_eq!(grey_to_rgba8([(40u8, 0xFF)].into_iter(), 4), vec![40, 40, 40, 0xFF]);
        // Grey with alpha keeps the alpha it was given.
        assert_eq!(grey_to_rgba8([(10u8, 20u8), (30, 40)].into_iter(), 8), vec![10, 10, 10, 20, 30, 30, 30, 40]);
    }

    /// A 16x16 gradient written by libheif, the encoder behind every HEIC
    /// tool that is not this one — so the decoder is checked against another
    /// implementation rather than against itself.
    ///
    /// Carried as text because the suite ships no binary fixtures, and small
    /// because a HEIC cannot be built from a few bytes of header the way a
    /// PNG can: its pixels are HEVC, and no Rust encoder for them builds
    /// within this crate's MSRV.
    /// A 64x64 10-bit HEIC holding the same shallow grey ramp as the AVIF
    /// fixtures: about a hundred sixteen-bit units per column, which 8 bits
    /// cannot express. Lossless, written by pillow-heif over libheif.
    const HEIC_RAMP_10BIT: &str = concat!(
        "AAAAHGZ0eXBoZWl4AAAAAG1pZjFoZWl4bWlhZgAAAVJtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAABdgABAAAAAAAAAM4AAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAAA0mlwcnAAAACzaXBjbwAAAHRodmNDAQQIAAAAAAAAAAAA//AA/P36+gAADwNgAAEAF0ABDAH//wQI",
        "AAADAJ24AAADAAD/ugJAYQABAClCAQEECAAAAwCduAAAAwAA/6AggQTZbqSSmubgIaDAgAAADIAAAAMAhGIAAQAGRAHBcYkS",
        "AAAAE2NvbHJuY2x4AAEADQAGgAAAABRpc3BlAAAAAAAAAEAAAABAAAAAEHBpeGkAAAAAAwoKCgAAABdpcG1hAAAAAAAAAAEA",
        "AQSBAgMEAAAA1m1kYXQAAADKKAGvBbgVevUg///6H/Q/z/5j4T4X4f4j4X4b4j4n4X4b4j4kAxwCm5k3ueNHjvlv8I/QAZeP",
        "z9X9AAADAJR1Bs5JsmHjLFhwQoAAD+woCeDK92AARx87ZCrCKzhs5cwAEDRlg3jzwAAWy6X7uYnO1YMksTfho0AARaWExemv",
        "DAADXQzjgqmAp74rrYAIYvom2YugABauUtFLQlptDj3wADPG1uq2JYCABCJLnLPD7RF1y+wANwn+ES8GAADhb1YvxT4pdck/",
        "N15LwA==",
    );

    /// The same ramp at 12 bits.
    const HEIC_RAMP_12BIT: &str = concat!(
        "AAAAHGZ0eXBoZWl4AAAAAG1pZjFoZWl4bWlhZgAAAVRtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAABeAABAAAAAAAAASoAAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAAA1GlwcnAAAAC1aXBjbwAAAHZodmNDAQQIAAAAAAAAAAAA//AA/P38/AAADwNgAAEAF0ABDAH//wQI",
        "AAADAJm4AAADAAD/ugJAYQABACtCAQEECAAAAwCZuAAAAwAA/6AggQRSlupJKa5uAhoMCAAAAwDIAAADAAhAYgABAAZEAcFx",
        "iRIAAAATY29scm5jbHgAAQANAAaAAAAAFGlzcGUAAAAAAAAAQAAAAEAAAAAQcGl4aQAAAAADDAwMAAAAF2lwbWEAAAAAAAAA",
        "AQABBIECAwQAAAEybWRhdAAAASYoAa8FuBAc/+Bm///7llNyZ9zLLbktPg2H7PQtPg2UPd4dSl2UPd4dlD3eHdHoMAdzVZ9Q",
        "BoekAdxwB3R3Or4AAQJI7ZLWN7OK6y8rrBgAApOerX7gH/nJLU0kth9iKXrAoAABl0ueBRa6TFsQZ+xAnAAIoVQ+7+VuLwqw",
        "aCrElg4AAaUz+ZTfTwVI8b8jxfAAEJPRtauoIQAdvax2+/wOMCU35OXAAI4OgqpcODxZNI0k0doACHGlTid+3f4kPhiQ+vDc",
        "ABlU7+p2fJn6bLpJstkAAhB9l1fGE3NPcvI9zE9F//AACGmy0LDDG0tTKwtMptAAzqC+DxHGKTvvWO+9lMAAB4stF7Q5z7x1",
        "FZfUUZAA7mqz6gDQ9IA7jgDubMf1xBLlgYA=",
    );

    const HEIC_GRADIENT: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAAXxtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAABoAABAAAAAAAAAMUAAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAAA/GlwcnAAAADcaXBjbwAAAHVodmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNw",
        "AAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAAAwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPB",
        "iQAAABNjb2xybmNseAABAA0ABoAAAAAUaXNwZQAAAAAAAABAAAAAQAAAAChjbGFwAAAAEAAAAAEAAAAQAAAAAf///9AAAAAC",
        "////0AAAAAIAAAAQcGl4aQAAAAADCAgIAAAAGGlwbWEAAAAAAAAAAQABBYECAwWEAAAAzW1kYXQAAADBKAGvBrIe4SSwkawM",
        "wY6ON9EJjG7hymaKZ/pf/3WrYjYL5EOMXoj/oUiSf/V4YmFoXp41sHLqVaifyq4sC4/ttdN2GzH9rdcqNdzCZA3yC2x2QxMy",
        "byTwBoM8oUSRLrSH4EbaR/9AZGEfAS+8Jyn/9J0//89t2z3s9KMylHQsoHew08RJD+KqEiWSI8PgIoxH0TPl0Wx6BM96P48E",
        "1DP93HbO0R8fhSMQwb1/WD6xg0OjqSXlrDVYtqDnMl3Ekl4ZoA==",
    );

    /// The same gradient, saved with an EXIF orientation asking for a quarter
    /// turn — which libheif also wrote into the container as an `irot`
    /// property, exactly as a phone does.
    const HEIC_GRADIENT_ROTATED: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAAcdtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAADRpbG9jAAAAAERAAAIAAQAAAAAB6wABAAAAAAAAAMUAAgAAAAACsAABAAAAAAAAACQAAAA4aWluZgAAAAAAAgAAABVp",
        "bmZlAgAAAAABAABodmMxAAAAABVpbmZlAgAAAQACAABFeGlmAAAAAA5waXRtAAAAAAABAAABBmlwcnAAAADlaXBjbwAAAHVo",
        "dmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNwAAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAA",
        "AwAAAwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPBiQAAABNjb2xybmNseAABAA0ABoAAAAAUaXNwZQAAAAAA",
        "AABAAAAAQAAAAChjbGFwAAAAEAAAAAEAAAAQAAAAAf///9AAAAAC////0AAAAAIAAAAQcGl4aQAAAAADCAgIAAAACWlyb3QD",
        "AAAAGWlwbWEAAAAAAAAAAQABBoECAwWEhgAAABppcmVmAAAAAAAAAA5jZHNjAAIAAQABAAAA8W1kYXQAAADBKAGvBrIe4SSw",
        "kawMwY6ON9EJjG7hymaKZ/pf/3WrYjYL5EOMXoj/oUiSf/V4YmFoXp41sHLqVaifyq4sC4/ttdN2GzH9rdcqNdzCZA3yC2x2",
        "QxMybyTwBoM8oUSRLrSH4EbaR/9AZGEfAS+8Jyn/9J0//89t2z3s9KMylHQsoHew08RJD+KqEiWSI8PgIoxH0TPl0Wx6BM96",
        "P48E1DP93HbO0R8fhSMQwb1/WD6xg0OjqSXlrDVYtqDnMl3Ekl4ZoAAAAAZFeGlmAABNTQAqAAAACAABARIAAwAAAAEABgAA",
        "AAAAAA==",
    );

    /// Decode base64 into the bytes of a fixture.
    ///
    /// Hand-written because the viewer has no use for base64 anywhere else,
    /// and a dependency carried solely to unpack two test files would be a
    /// dependency in everyone's build.
    fn from_base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        let mut out = Vec::new();
        let mut accumulator: u32 = 0;
        let mut bits = 0;
        for byte in text.bytes().filter(|byte| *byte != b'=') {
            let value = ALPHABET.iter().position(|candidate| *candidate == byte).expect("a base64 character") as u32;
            accumulator = (accumulator << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((accumulator >> bits) as u8);
            }
        }
        out
    }

    /// The colour at a pixel, ignoring alpha.
    fn pixel_at(image: &DecodedImage, x: u32, y: u32) -> [u8; 3] {
        let start = ((y * image.width + x) * 4) as usize;
        [image.pixels[start], image.pixels[start + 1], image.pixels[start + 2]]
    }

    /// The format a modern phone photographs in, opened end to end.
    #[test]
    fn decodes_heic_to_rgba() {
        let loaded = decode(&from_base64(HEIC_GRADIENT)).unwrap();
        assert_eq!((loaded.image.width, loaded.image.height), (16, 16));
        assert_eq!(loaded.image.pixels.len(), 16 * 16 * 4);
        assert!(
            loaded.image.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0xFF),
            "an opaque HEIC decoded with transparency"
        );

        // The fixture is a gradient: red rises to the right, green downwards.
        // Compared with a wide margin because HEVC stores colour at half
        // resolution, so a corner is not exactly the value encoded there.
        let top_left = pixel_at(&loaded.image, 0, 0);
        let top_right = pixel_at(&loaded.image, 15, 0);
        let bottom_left = pixel_at(&loaded.image, 0, 15);
        assert!(
            top_right[0] > top_left[0] + 100,
            "red did not rise across the image: {top_left:?} to {top_right:?}"
        );
        assert!(
            bottom_left[1] > top_left[1] + 100,
            "green did not rise down the image: {top_left:?} to {bottom_left:?}"
        );
    }

    /// A flat green written by libheif with an ICC profile and no CICP codes.
    ///
    /// This is the shape of file the decoder gets wrong on its own: it notices
    /// the profile, does not read it, and falls back to matrix coefficients
    /// that are not the ones the pixels were coded with.
    const HEIC_WITH_ICC: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAA8FtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAAD5QABAAAAAAAAADYAAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAADQWlwcnAAAAMhaXBjbwAAAHVodmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNw",
        "AAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAAAwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPB",
        "iQAAAlhjb2xycHJvZgAAAkxsY21zBEAAAG1udHJSR0IgWFlaIAfqAAgAFQAVABgAJWFjc3BNU0ZUAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAD21gABAAAAANMtbGNtcwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "C2Rlc2MAAAEIAAAANmNwcnQAAAFAAAAATHd0cHQAAAGMAAAAFGNoYWQAAAGgAAAALHJYWVoAAAHMAAAAFGJYWVoAAAHgAAAA",
        "FGdYWVoAAAH0AAAAFHJUUkMAAAIIAAAAIGdUUkMAAAIIAAAAIGJUUkMAAAIIAAAAIGNocm0AAAIoAAAAJG1sdWMAAAAAAAAA",
        "AQAAAAxlblVTAAAAGgAAABwAcwBSAEcAQgAgAGIAdQBpAGwAdAAtAGkAbgAAbWx1YwAAAAAAAAABAAAADGVuVVMAAAAwAAAA",
        "HABOAG8AIABjAG8AcAB5AHIAaQBnAGgAdAAsACAAdQBzAGUAIABmAHIAZQBlAGwAeVhZWiAAAAAAAAD21gABAAAAANMtc2Yz",
        "MgAAAAAAAQxCAAAF3v//8yUAAAeTAAD9kP//+6H///2iAAAD3AAAwG5YWVogAAAAAAAAb6AAADj1AAADkFhZWiAAAAAAAAAk",
        "nwAAD4QAALbDWFlaIAAAAAAAAGKXAAC3hwAAGNlwYXJhAAAAAAADAAAAAmZmAADypwAADVkAABPQAAAKW2Nocm0AAAAAAAMA",
        "AAAAo9cAAFR7AABMzQAAmZoAACZmAAAPXAAAABRpc3BlAAAAAAAAAEAAAABAAAAAKGNsYXAAAAAQAAAAAQAAABAAAAAB////",
        "0AAAAAL////QAAAAAgAAABBwaXhpAAAAAAMICAgAAAAYaXBtYQAAAAAAAAABAAEFgQIDBYQAAAA+bWRhdAAAADIoAa8GshZn",
        "NJICQ+mv///CYf//0qlAFrA2X2/oZsTkstqBHPph8xg6licaI3CtwJy7wA==",
    );

    /// The colour of an ICC-tagged HEIC must match what libheif reads from the
    /// same file, not what the decoder produces when it guesses.
    ///
    /// The fixture is a flat (10, 200, 90). Left to itself the decoder returns
    /// roughly (0, 185, 85) — visibly wrong, and wrong in a way no amount of
    /// colour management afterwards can undo.
    #[test]
    fn an_icc_tagged_heic_decodes_to_the_colour_it_was_encoded_with() {
        let loaded = decode(&from_base64(HEIC_WITH_ICC)).unwrap();

        let pixel = &loaded.image.pixels[..3];
        for (channel, expected) in pixel.iter().zip([10u8, 200, 90]) {
            let difference = channel.abs_diff(expected);
            assert!(
                difference <= 6,
                "the flat colour came back as {pixel:?}, not the (10, 200, 90) it was encoded with"
            );
        }
    }

    /// And the profile reaches the viewer, so the colour is finished on the
    /// GPU like every other tagged format rather than resolved on the way in.
    #[test]
    fn an_icc_tagged_heic_carries_its_profile_to_the_shader() {
        let loaded = decode(&from_base64(HEIC_WITH_ICC)).unwrap();
        assert!(
            loaded.profile.is_some(),
            "the ICC profile was dropped, so the image would be shown in the wrong space"
        );
    }

    /// A HEIC stating CICP codes is the ordinary case and must not be touched:
    /// the decoder resolves its colour, and attaching a profile on top would
    /// convert the pixels a second time.
    #[test]
    fn a_cicp_tagged_heic_is_still_left_to_the_decoder() {
        assert!(decode(&from_base64(HEIC_GRADIENT)).unwrap().profile.is_none());
    }

    /// A 64x48 gradient with a 16-pixel thumbnail beside it, as libheif writes
    /// one — the same arrangement a phone produces.
    const HEIC_WITH_THUMBNAIL: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAAmJtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAADRpbG9jAAAAAERAAAIAAQAAAAAChgABAAAAAAAAAfkAAgAAAAAEfwABAAAAAAAAAKsAAAA4aWluZgAAAAAAAgAAABVp",
        "bmZlAgAAAAABAABodmMxAAAAABVpbmZlAgAAAAACAABodmMxAAAAAA5waXRtAAAAAAABAAABoWlwcnAAAAF6aXBjbwAAAHZo",
        "dmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNwAAADAJAAAAMAAAMAHroCQGEAAQAqQgEBA3AAAAMAkAAA",
        "AwAAAwAeoCCBBZbq5Ka5uAhoMCAAAAMDIAAAAwAhYgABAAZEAcFzwIkAAAATY29scm5jbHgAAQANAAaAAAAAFGlzcGUAAAAA",
        "AAAAQAAAAEAAAAAoY2xhcAAAAEAAAAABAAAAMAAAAAEAAAAAAAAAAv////AAAAACAAAAEHBpeGkAAAAAAwgICAAAAHVodmND",
        "AQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNwAAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAA",
        "AwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPBiQAAAChjbGFwAAAAEAAAAAEAAAAMAAAAAf///9AAAAAC////",
        "zAAAAAIAAAAfaXBtYQAAAAAAAAACAAEFgQIDBYQAAgSGAwWHAAAAGmlyZWYAAAAAAAAADnRobWIAAgABAAEAAAKsbWRhdAAA",
        "AfUoAa8GOOllh0y24R+1b38FdWdZTuD5dwhtN6PJqe1YKIOOvBc8+ihN9TE+OXBSE9SxAP0GxfEU/d//MaUXxds9jzT71tY6",
        "UaNk0p//RTlyWRIWPKCJX0/14eyMZXkgmRdNLxm1/BDztF32wn8nvh803+VOL7mR7pTmrrecO9tn/K9XXdx96RWIWiVgq+5J",
        "J97xVdz7AGPjwo/9neVw6g8/Jxp5ehIYYWZtk5O3C3drDDaiRQnutkZ/vkPIENf4h4TXmPxkBZ0vuv8I8xYTlI835zebdZm1",
        "ys4Cc/+Twtm27YrvSzx6fj2nTn0B+Q4SZOhDzPTW/FtJVEcdX/h//wWjQCSdOrNwNJV/20ifUy4yYBQc3EDL0oFOmsDJkJR2",
        "LeJ2WLsAQe9jWj3Zf2z6q5YV4Mks9tPsn/If5BKWH8LCxqPuficC95A7bkvpO/6CB0Xqi9JN6HKiLM1AXBKUJ/d8WH9vFQk8",
        "Ocf3f7relkYi87LGZWka/Ly/aeuCZ/3P5H6bxpBGfTSSKog2AOvGaeqckqkXiOL3AeFxFbt5TRL/DWid3gaIscpUw565rh4N",
        "ZIzUyQGEG6V2q6oT7M9JCaE7ZydC2Y64HjoODeBwqxvBBsBIDvGxqMQDwRPg1ecELx701Jbs/WIgQ9nV3yN80l6jmX1bK/4A",
        "AACnKAGvBjIeyRRQiQKs/NTUAT6FlCZD6UbwABv5nF56+QMX/gPVP0xOkl3jXeehAf9tmAXeka/8Joxx+T9T9KM6U8Gh6Vhl",
        "Rb3H+A606130rjgTHYIETU5ovmwv6o2jez/qvl9Acs/wfYJ11sZlrK/i4eU6199ApGfuY14gKMQU9L0v04HBj5VS2bftL+We",
        "Re971BP/RVjqIPldaiYgJ66SerO98+4BMLg=",
    );

    /// The quick frame of a HEIC comes from the thumbnail item the encoder
    /// stored beside the picture — not from EXIF, which a HEIC does not carry
    /// one in.
    #[test]
    fn a_heic_thumbnail_is_found_and_decoded() {
        let bytes = from_base64(HEIC_WITH_THUMBNAIL);
        let thumbnail = decode_thumbnail(&bytes).expect("the thumbnail was not found");

        assert_eq!(thumbnail.fidelity, Fidelity::Thumbnail);
        // Smaller than the picture it stands in for, which is the whole point:
        // it decodes in a fraction of the time.
        assert!(
            thumbnail.image.width < 64 && thumbnail.image.height < 48,
            "the full image came back instead of the thumbnail: {}x{}",
            thumbnail.image.width,
            thumbnail.image.height
        );
        assert_eq!(thumbnail.image.pixels.len(), (thumbnail.image.width * thumbnail.image.height * 4) as usize);
    }

    /// The quick frame must look like the picture it stands in for. A
    /// thumbnail showing something else — the wrong item, or bytes read at the
    /// wrong offset — would be worse than no quick frame at all.
    #[test]
    fn the_thumbnail_looks_like_the_image_it_stands_in_for() {
        let bytes = from_base64(HEIC_WITH_THUMBNAIL);
        let thumbnail = decode_thumbnail(&bytes).expect("the thumbnail was not found");
        let full = decode(&bytes).expect("the full image");

        // The fixture is a gradient: red rises to the right, green downwards.
        // Both pictures must agree about that, with a wide margin for the
        // scale difference and for chroma stored at half resolution.
        let corner = |image: &DecodedImage, x: u32, y: u32| {
            let start = ((y * image.width + x) * 4) as usize;
            [image.pixels[start], image.pixels[start + 1], image.pixels[start + 2]]
        };

        let small = corner(&thumbnail.image, thumbnail.image.width - 1, 0);
        let large = corner(&full.image, full.image.width - 1, 0);
        for channel in 0..3 {
            let difference = small[channel].abs_diff(large[channel]);
            assert!(difference < 60, "the top-right corners disagree: {small:?} against {large:?}");
        }
    }

    /// Most HEICs carry no thumbnail, and that is not an error: the viewer
    /// waits for the full decode, as it did before.
    #[test]
    fn a_heic_without_a_thumbnail_has_no_quick_frame() {
        assert!(decode_thumbnail(&from_base64(HEIC_GRADIENT)).is_none());
    }

    /// The quick frame reaches the same code that reads the file, so a
    /// damaged one must come back as no thumbnail rather than as a panic.
    #[test]
    fn a_broken_heic_yields_no_thumbnail_rather_than_panicking() {
        let whole = from_base64(HEIC_WITH_THUMBNAIL);
        for cut in [0, 1, 12, 40, 600, whole.len() / 2, whole.len() - 1] {
            let _ = decode_thumbnail(&whole[..cut.min(whole.len())]);
        }

        let mut damaged = whole.clone();
        for index in (0..damaged.len()).step_by(23) {
            damaged[index] ^= 0xA5;
        }
        let _ = decode_thumbnail(&damaged);
    }

    /// A HEIC states its rotation in the container, and the decoder applies
    /// it. An encoder writing a photograph puts the same rotation in EXIF, so
    /// honouring the tag as well would turn a portrait photograph on its side.
    #[test]
    fn a_heic_is_not_rotated_a_second_time() {
        let loaded = decode(&from_base64(HEIC_GRADIENT_ROTATED)).unwrap();
        assert_eq!(
            loaded.orientation,
            Orientation::Normal,
            "the EXIF rotation was applied on top of the rotation the container already carried"
        );

        // The file asks for a quarter turn and the pixels arrive already
        // turned: what was the bottom-left corner of the gradient is now the
        // top-left one.
        let upright = decode(&from_base64(HEIC_GRADIENT)).unwrap();
        let turned_corner = pixel_at(&loaded.image, 0, 0);
        let original_corner = pixel_at(&upright.image, 0, 15);
        for channel in 0..3 {
            let difference = turned_corner[channel].abs_diff(original_corner[channel]);
            assert!(
                difference < 20,
                "the rotated image is not the upright one turned: {turned_corner:?} against {original_corner:?}"
            );
        }
    }

    /// The same gradient with an ICC profile embedded in the container, which
    /// is how a phone tags a photograph as Display P3.
    const HEIC_GRADIENT_TAGGED: &str = concat!(
        "AAAAHGZ0eXBoZWljAAAAAG1pZjFoZWljbWlhZgAAA8FtZXRhAAAAAAAAACFoZGxyAAAAAAAAAABwaWN0AAAAAAAAAAAAAAAA",
        "AAAAACJpbG9jAAAAAERAAAEAAQAAAAAD5QABAAAAAAAAAMUAAAAjaWluZgAAAAAAAQAAABVpbmZlAgAAAAABAABodmMxAAAA",
        "AA5waXRtAAAAAAABAAADQWlwcnAAAAMhaXBjbwAAAHVodmNDAQNwAAAAAAAAAAAAHvAA/P34+AAADwNgAAEAGEABDAH//wNw",
        "AAADAJAAAAMAAAMAHroCQGEAAQApQgEBA3AAAAMAkAAAAwAAAwAeoCCBBZbqrprm4CGgwIAAAAyAAAADAIRiAAEABkQBwXPB",
        "iQAAAlhjb2xycHJvZgAAAkxsY21zBEAAAG1udHJSR0IgWFlaIAfqAAgAEwAUAAAAMWFjc3BNU0ZUAAAAAAAAAAAAAAAAAAAA",
        "AAAAAAAAAAAAAAD21gABAAAAANMtbGNtcwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "C2Rlc2MAAAEIAAAANmNwcnQAAAFAAAAATHd0cHQAAAGMAAAAFGNoYWQAAAGgAAAALHJYWVoAAAHMAAAAFGJYWVoAAAHgAAAA",
        "FGdYWVoAAAH0AAAAFHJUUkMAAAIIAAAAIGdUUkMAAAIIAAAAIGJUUkMAAAIIAAAAIGNocm0AAAIoAAAAJG1sdWMAAAAAAAAA",
        "AQAAAAxlblVTAAAAGgAAABwAcwBSAEcAQgAgAGIAdQBpAGwAdAAtAGkAbgAAbWx1YwAAAAAAAAABAAAADGVuVVMAAAAwAAAA",
        "HABOAG8AIABjAG8AcAB5AHIAaQBnAGgAdAAsACAAdQBzAGUAIABmAHIAZQBlAGwAeVhZWiAAAAAAAAD21gABAAAAANMtc2Yz",
        "MgAAAAAAAQxCAAAF3v//8yUAAAeTAAD9kP//+6H///2iAAAD3AAAwG5YWVogAAAAAAAAb6AAADj1AAADkFhZWiAAAAAAAAAk",
        "nwAAD4QAALbDWFlaIAAAAAAAAGKXAAC3hwAAGNlwYXJhAAAAAAADAAAAAmZmAADypwAADVkAABPQAAAKW2Nocm0AAAAAAAMA",
        "AAAAo9cAAFR7AABMzQAAmZoAACZmAAAPXAAAABRpc3BlAAAAAAAAAEAAAABAAAAAKGNsYXAAAAAQAAAAAQAAABAAAAAB////",
        "0AAAAAL////QAAAAAgAAABBwaXhpAAAAAAMICAgAAAAYaXBtYQAAAAAAAAABAAEFgQIDBYQAAADNbWRhdAAAAMEoAa8Gsh7h",
        "JLCRrAzBjo430QmMbuHKZopn+l//datiNgvkQ4xeiP+hSJJ/9XhiYWhenjWwcupVqJ/KriwLj+2103YbMf2t1yo13MJkDfIL",
        "bHZDEzJvJPAGgzyhRJEutIfgRtpH/0BkYR8BL7wnKf/0nT//z23bPez0ozKUdCygd7DTxEkP4qoSJZIjw+AijEfRM+XRbHoE",
        "z3o/jwTUM/3cds7RHx+FIxDBvX9YPrGDQ6OpJeWsNVi2oOcyXcSSXhmg",
    );

    /// A HEIC's colour is finished in one of two places, and which one depends
    /// on how the file describes itself.
    ///
    /// With CICP codes the decoder resolves it and the pixels arrive as sRGB,
    /// so nothing may be attached — applying a profile on top would convert
    /// them twice (ADR 0007). With an ICC profile the decoder resolves
    /// nothing, so the profile is carried through to the shader instead.
    ///
    /// Both halves are asserted here: the first alone would pass for a build
    /// that never reads a profile at all.
    #[test]
    fn a_heic_carries_a_profile_only_when_the_decoder_did_not_use_one() {
        // Stated with CICP codes: resolved on the way in, nothing to attach.
        assert!(decode(&from_base64(HEIC_GRADIENT)).unwrap().profile.is_none());

        // Stated with an ICC profile: carried to the shader.
        let tagged = from_base64(HEIC_GRADIENT_TAGGED);
        assert!(
            tagged.windows(4).any(|window| window == b"prof"),
            "the fixture is meant to carry an ICC profile"
        );
        assert!(
            decode(&tagged).unwrap().profile.is_some(),
            "an ICC-tagged HEIC lost its profile, so it would be shown in whatever space the decoder guessed"
        );
    }

    /// A decoder reading a hostile file is the reason the sandbox exists; this
    /// one is Rust, so a broken file must come back as an error rather than
    /// taking the viewer with it.
    #[test]
    fn a_broken_heic_is_an_error_rather_than_a_panic() {
        let whole = from_base64(HEIC_GRADIENT);
        for cut in [0, 1, 12, 40, whole.len() / 2, whole.len() - 1] {
            assert!(decode(&whole[..cut]).is_err(), "a HEIC cut to {cut} bytes decoded anyway");
        }

        // Bytes flipped throughout the file: the decoder may refuse it or
        // produce nonsense pixels, but it must not take the viewer down.
        let mut damaged = whole.clone();
        for index in (0..damaged.len()).step_by(29) {
            damaged[index] ^= 0xA5;
        }
        let _ = decode(&damaged);
    }

    /// jxl-oxide applies the header orientation itself. The viewer must not
    /// apply the EXIF tag as well, or such an image is turned twice.
    #[test]
    fn a_jpeg_xl_is_not_rotated_a_second_time() {
        let loaded = decode(&encode_jxl(6, 4, None)).unwrap();
        assert_eq!(loaded.orientation, Orientation::Normal);
        assert_eq!(loaded.display_size(), (6, 4));
    }

    #[test]
    fn widening_rgb_samples_adds_opaque_alpha() {
        assert_eq!(rgb_to_rgba8(&[10, 20, 30], 4), vec![10, 20, 30, 0xFF]);
        assert_eq!(rgb_to_rgba8(&[1, 2, 3, 4, 5, 6], 8), vec![1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
    }

    /// Dimensions come from the file and may be a lie; the multiplication that
    /// sizes the buffer must not wrap.
    #[test]
    fn absurd_dimensions_are_refused_rather_than_overflowing() {
        assert!(pixel_count(u32::MAX, u32::MAX, Depth::Eight).is_err());
        assert!(pixel_count(0, 0, Depth::Eight).is_err());
        assert_eq!(pixel_count(2, 3, Depth::Eight).unwrap(), 24);
        assert_eq!(pixel_count(2, 3, Depth::Sixteen).unwrap(), 48);
    }

    #[test]
    fn orientation_swaps_axes_only_when_rotated_by_a_quarter_turn() {
        assert!(!Orientation::Normal.swaps_axes());
        assert!(!Orientation::Rotate180.swaps_axes());
        assert!(Orientation::Rotate90.swaps_axes());
        assert!(Orientation::Rotate270.swaps_axes());
        assert!(Orientation::Transpose.swaps_axes());
    }

    /// Every orientation, so a property can be checked against all of them
    /// rather than against the two or three a test author thinks of.
    const ALL: [Orientation; 8] = [
        Orientation::Normal,
        Orientation::FlipHorizontal,
        Orientation::Rotate180,
        Orientation::FlipVertical,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
    ];

    /// The eight orientations are closed under composition — which is what
    /// lets `then` return one of them rather than needing a fallback. The
    /// `unwrap_or` in it is unreachable, and this is what says so.
    #[test]
    fn composing_any_two_orientations_gives_another_one() {
        for first in ALL {
            for second in ALL {
                let product = first.then(second);
                // `from_matrix` found it, which is only true if the product
                // really is one of the eight rather than the fallback.
                assert_eq!(
                    product.matrix(),
                    matrix_product(first.matrix(), second.matrix()),
                    "{first:?} then {second:?} fell back instead of composing",
                );
            }
        }
    }

    fn matrix_product(a: [[i8; 2]; 2], b: [[i8; 2]; 2]) -> [[i8; 2]; 2] {
        [
            [a[0][0] * b[0][0] + a[0][1] * b[1][0], a[0][0] * b[0][1] + a[0][1] * b[1][1]],
            [a[1][0] * b[0][0] + a[1][1] * b[1][0], a[1][0] * b[0][1] + a[1][1] * b[1][1]],
        ]
    }

    /// Four quarter turns is where you started, from any orientation. This is
    /// the property that would catch a sign error in the composition, which is
    /// how the equivalent geometry went wrong in v0.15.0.
    #[test]
    fn four_quarter_turns_return_to_the_start() {
        for start in ALL {
            for clockwise in [true, false] {
                let mut turned = start;
                for _ in 0..4 {
                    turned = turned.turned(clockwise);
                }
                assert_eq!(turned, start, "{start:?} did not come back after four turns");
            }
        }
    }

    /// Turning one way then the other is turning not at all.
    #[test]
    fn a_turn_and_its_opposite_cancel() {
        for start in ALL {
            assert_eq!(start.turned(true).turned(false), start, "{start:?} did not come back");
            assert_eq!(start.turned(false).turned(true), start, "{start:?} did not come back");
        }
    }

    /// A quarter turn exchanges the axes; two of them do not. Stated as a
    /// property so it holds for the mirrored orientations too, where the
    /// intuition from a photograph runs out.
    #[test]
    fn a_quarter_turn_exchanges_the_axes_and_a_half_turn_does_not() {
        for start in ALL {
            assert_ne!(
                start.turned(true).swaps_axes(),
                start.swaps_axes(),
                "a quarter turn of {start:?} kept the same axes"
            );
            assert_eq!(
                start.turned(true).turned(true).swaps_axes(),
                start.swaps_axes(),
                "a half turn of {start:?} exchanged the axes",
            );
        }
    }

    /// The turn a user asks for lands where they can see it: turning an
    /// upright picture clockwise is `Rotate90`, and turning it the other way
    /// is `Rotate270`. The mirrored cases are held by the properties above,
    /// which is where a hand-written table would have gone wrong.
    #[test]
    fn turning_an_upright_picture_goes_the_way_it_says() {
        assert_eq!(Orientation::Normal.turned(true), Orientation::Rotate90);
        assert_eq!(Orientation::Normal.turned(false), Orientation::Rotate270);
        assert_eq!(Orientation::Rotate90.turned(true), Orientation::Rotate180);
    }

    /// Composing with `Normal` changes nothing — the case the viewer is in
    /// whenever the user has not turned anything, which is nearly always.
    #[test]
    fn composing_with_normal_is_the_identity() {
        for start in ALL {
            assert_eq!(start.then(Orientation::Normal), start);
            assert_eq!(Orientation::Normal.then(start), start);
        }
    }

    #[test]
    fn orientation_maps_every_exif_value() {
        assert_eq!(Orientation::from_exif(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif(8), Orientation::Rotate270);
        // Out-of-range values appear in the wild; upright is the safe reading.
        assert_eq!(Orientation::from_exif(0), Orientation::Normal);
        assert_eq!(Orientation::from_exif(99), Orientation::Normal);
    }

    /// Three 4x4 frames — red, green, blue — written by Pillow, an encoder
    /// independent of every decoder under test. 200 ms per frame.
    const ANIMATED_GIF: &str = concat!(
        "R0lGODlhBAAEAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQAFAAAACwAAAAABAAEAAAICQABCBxIsCCA",
        "gAAh+QQBFAABACwAAAAABAAEAIEA/wAAAAAAAAAAAAAICQABCBxIsCCAgAAh+QQBFAABACwAAAAABAAEAIEAAP8AAAAAAAAA",
        "AAAICQABCBxIsCCAgAA7",
    );

    /// The same three frames with a stated delay of zero, which files use to
    /// mean "you pick" and browsers read as 100 ms.
    const ANIMATED_GIF_ZERO_DELAY: &str = concat!(
        "R0lGODlhBAAEAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAsAAAAAAQABAAACAkAAQgcSLAggIAAIfkEAQAA",
        "AQAsAAAAAAQABACBAP8AAAAAAAAAAAAACAkAAQgcSLAggIAAIfkEAQAAAQAsAAAAAAQABACBAAD/AAAAAAAAAAAACAkAAQgc",
        "SLAggIAAOw==",
    );

    /// The same three frames as an APNG, 100 ms per frame, from Pillow.
    const ANIMATED_APNG: &str = concat!(
        "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAIAAAAmkwkpAAAACGFjVEwAAAADAAAAAM7tusAAAAAaZmNUTAAAAAAAAAAEAAAA",
        "BAAAAAAAAAAAAAEACgAAV3ID4QAAABFJREFUeJxj/M+AAExIbDwcADPRAQcw5+FMAAAAGmZjVEwAAAABAAAABAAAAAQAAAAA",
        "AAAAAAABAAoAAMwB6TUAAAAWZmRBVAAAAAJ4nGNk+M8AB0wIJj4OADLSAQcatUejAAAAGmZjVEwAAAADAAAABAAAAAQAAAAA",
        "AAAAAAABAAoAACGXOtwAAAAXZmRBVAAAAAR4nGNkYPjPAANMcBZeDgAx0wEH5WGlLAAAAABJRU5ErkJggg==",
    );

    /// The same three frames as a lossless animated WebP, 150 ms per frame,
    /// from Pillow.
    const ANIMATED_WEBP: &str = concat!(
        "UklGRrQAAABXRUJQVlA4WAoAAAACAAAAAwAAAwAAQU5JTQYAAAAAAAAAAABBTk1GKAAAAAAAAAAAAAMAAAMAAJYAAAJWUDhM",
        "DwAAAC8DwAAABxD9j/4HIqL/AQBBTk1GKAAAAAAAAAAAAAMAAAMAAJYAAABWUDhMDwAAAC8DwAAAB9D/iP4HIqL/AQBBTk1G",
        "KAAAAAAAAAAAAAMAAAMAAJYAAABWUDhMDwAAAC8DwAAABxDR//4HIqL/AQA=",
    );

    /// Every pixel of a frame, asserted against one flat colour.
    fn assert_flat(image: &DecodedImage, expected: [u8; 3], which: &str) {
        for pixel in image.pixels.as_chunks::<4>().0.iter() {
            assert_eq!(&pixel[..3], &expected, "the {which} frame is not the flat colour it was encoded as");
        }
    }

    /// An animated GIF plays: every frame decoded, in order, with its delay.
    /// Red, green, blue is the order the file was written in, so a decoder
    /// that shuffled or recomposited frames wrongly fails on colour.
    #[test]
    fn an_animated_gif_carries_every_frame() {
        let loaded = decode(&from_base64(ANIMATED_GIF)).unwrap();
        let animation = loaded.animation.as_ref().expect("an animated GIF should carry its frames");

        assert_eq!(animation.frames.len(), 3);
        assert_flat(&animation.frames[0].image, [255, 0, 0], "first");
        assert_flat(&animation.frames[1].image, [0, 255, 0], "second");
        assert_flat(&animation.frames[2].image, [0, 0, 255], "third");
        for frame in &animation.frames {
            assert_eq!(frame.delay, std::time::Duration::from_millis(200));
        }

        // The still `image` is the first frame: a caller that ignores the
        // animation shows exactly what earlier versions showed.
        assert_eq!(loaded.image.pixels, animation.frames[0].image.pixels);
    }

    /// A zero delay means "unspecified", and plays at the 100 ms every
    /// browser settled on — not as fast as the machine can flip frames.
    #[test]
    fn a_zero_gif_delay_plays_at_the_browser_default() {
        let loaded = decode(&from_base64(ANIMATED_GIF_ZERO_DELAY)).unwrap();
        let animation = loaded.animation.expect("the GIF should carry its frames");
        for frame in &animation.frames {
            assert_eq!(frame.delay, std::time::Duration::from_millis(100));
        }
    }

    #[test]
    fn an_animated_png_carries_every_frame() {
        let loaded = decode(&from_base64(ANIMATED_APNG)).unwrap();
        let animation = loaded.animation.as_ref().expect("an APNG should carry its frames");

        assert_eq!(animation.frames.len(), 3);
        assert_flat(&animation.frames[0].image, [255, 0, 0], "first");
        assert_flat(&animation.frames[2].image, [0, 0, 255], "third");
        for frame in &animation.frames {
            assert_eq!(frame.delay, std::time::Duration::from_millis(100));
        }
    }

    /// Every pixel of a frame, within `tolerance` of one flat colour.
    fn assert_flat_within(image: &DecodedImage, expected: [u8; 3], tolerance: u8, which: &str) {
        for pixel in image.pixels.as_chunks::<4>().0.iter() {
            for (channel, want) in pixel[..3].iter().zip(expected) {
                assert!(
                    channel.abs_diff(want) <= tolerance,
                    "the {which} frame decoded to {pixel:?}, not the flat {expected:?} it was encoded as"
                );
            }
        }
    }

    /// Off by one, not exact, and the reason is named: `image-webp` blends a
    /// frame onto the canvas with a scale of 2^24/255 rounded down, so a
    /// fully opaque blended frame loses one part in 255 — a 255 channel
    /// comes back 254 (`alpha_blending.rs::blend_channel_nonpremult`).
    /// Pillow reads the same file exactly, so the file is right and the
    /// decoder is a hair dark. Invisible in practice; recorded here rather
    /// than hidden behind a looser assertion without a reason.
    #[test]
    fn an_animated_webp_carries_every_frame() {
        let loaded = decode(&from_base64(ANIMATED_WEBP)).unwrap();
        let animation = loaded.animation.as_ref().expect("an animated WebP should carry its frames");

        assert_eq!(animation.frames.len(), 3);
        assert_flat(&animation.frames[0].image, [255, 0, 0], "first");
        assert_flat_within(&animation.frames[1].image, [0, 255, 0], 1, "second");
        assert_flat_within(&animation.frames[2].image, [0, 0, 255], 1, "third");
        for frame in &animation.frames {
            assert_eq!(frame.delay, std::time::Duration::from_millis(150));
        }
    }

    /// Stills stay stills: a one-frame GIF, an ordinary PNG and an ordinary
    /// WebP carry no animation, so nothing ever ticks for them.
    #[test]
    fn still_images_carry_no_animation() {
        assert!(decode(&encode(image::ImageFormat::Gif, 4, 4)).unwrap().animation.is_none());
        assert!(decode(&encode(image::ImageFormat::Png, 4, 4)).unwrap().animation.is_none());
        assert!(decode(&encode_webp(4, 4, None)).unwrap().animation.is_none());
    }

    #[test]
    fn display_size_follows_orientation() {
        let loaded = LoadedImage {
            image: DecodedImage {
                width: 100,
                height: 50,
                pixels: Vec::new(),
                depth: Depth::Eight,
            },
            orientation: Orientation::Rotate90,
            fidelity: Fidelity::Full,
            // Irrelevant to this test; any variant would do.
            format: Format::Png,
            metadata: Default::default(),
            profile: None,
            vector: None,
            animation: None,
        };
        assert_eq!(loaded.display_size(), (50, 100));
    }

    #[test]
    fn supported_extensions_are_matched_case_insensitively() {
        assert!(is_supported(Path::new("photo.JPG")));
        assert!(is_supported(Path::new("photo.png")));
        assert!(!is_supported(Path::new("notes.txt")));
        assert!(!is_supported(Path::new("no-extension")));
    }

    /// A 10-bit HEIC keeps its depth: the decoder hands over 16-bit samples
    /// and they reach `DecodedImage` unnarrowed. The ramp fixture climbs in
    /// steps an 8-bit pipeline cannot take — its quantum is 257 — so one
    /// small non-zero step proves the bits survived.
    #[test]
    fn a_ten_bit_heic_keeps_more_than_eight_bits() {
        let loaded = decode_here(&from_base64(HEIC_RAMP_10BIT)).unwrap();
        assert_eq!(loaded.image.depth, Depth::Sixteen);
        assert_eq!((loaded.image.width, loaded.image.height), (64, 64));

        let red = |x: u32| {
            let start = ((32 * loaded.image.width + x) * 4 * 2) as usize;
            u16::from_ne_bytes([loaded.image.pixels[start], loaded.image.pixels[start + 1]])
        };
        let row: Vec<u16> = (0..64).map(red).collect();
        assert!(row.windows(2).all(|pair| pair[1] >= pair[0]), "the ramp must climb: {row:?}");

        let small_steps = row.windows(2).filter(|pair| pair[1] > pair[0] && pair[1] - pair[0] < 200).count();
        assert!(small_steps > 0, "every step is 8-bit sized, so the depth was narrowed somewhere: {row:?}");
    }

    #[test]
    fn a_twelve_bit_heic_keeps_more_than_eight_bits() {
        let loaded = decode_here(&from_base64(HEIC_RAMP_12BIT)).unwrap();
        assert_eq!(loaded.image.depth, Depth::Sixteen);

        let red = |x: u32| {
            let start = ((32 * loaded.image.width + x) * 4 * 2) as usize;
            u16::from_ne_bytes([loaded.image.pixels[start], loaded.image.pixels[start + 1]])
        };
        let row: Vec<u16> = (0..64).map(red).collect();
        let small_steps = row.windows(2).filter(|pair| pair[1] > pair[0] && pair[1] - pair[0] < 200).count();
        assert!(small_steps > 0, "every step is 8-bit sized, so the depth was narrowed somewhere: {row:?}");
    }
}
