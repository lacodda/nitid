//! Decoding an image file into pixels the renderer can upload.
//!
//! Every decoder in this module is pure Rust: a malformed file costs a panic or
//! an error, never code execution. That now includes HEIC, which turned out to
//! have a Rust decoder and so never reached the sandbox built for it — see
//! `docs/adr/0007-heic-decodes-in-rust.md`. AVIF still has no such decoder and
//! is the format the sandbox of `docs/adr/0002-sandbox-c-decoders.md` awaits.

use std::fs;
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

/// A decoded image: tightly packed RGBA8 rows, ready for a GPU upload.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub pixels: Vec<u8>,
}

impl DecodedImage {
    /// Bytes per row as the GPU sees them.
    pub fn bytes_per_row(&self) -> u32 {
        self.width * 4
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
    fn from_exif(value: u16) -> Self {
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

    /// Whether the transform exchanges width and height.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270)
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

/// Read a file from disk and decode it.
pub fn load(path: &Path) -> Result<LoadedImage> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    decode(&bytes).with_context(|| format!("decoding {}", path.display()))
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
        let image = crate::sandbox::decode(bytes).with_context(|| format!("decoding the {}", format.name()))?;
        return Ok(LoadedImage {
            image,
            orientation: if format.orients_itself() {
                Orientation::Normal
            } else {
                read_orientation(bytes)
            },
            fidelity: Fidelity::Full,
            profile: color::profile_from(bytes),
            vector: None,
        });
    }

    let malformed = || format!("the {} data is malformed", format.name());

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
            profile: None,
            vector: Some(vector),
        });
    }

    // JPEG XL comes back with its profile attached: one pass over the file
    // yields both, where reading the profile separately — as the other formats
    // do — would mean decoding the image twice.
    let (image, profile) = match format {
        Format::JpegXl => decode_jxl(bytes).with_context(malformed)?,
        Format::Jpeg => (decode_jpeg(bytes).with_context(malformed)?, color::profile_from(bytes)),
        Format::WebP => (decode_webp(bytes).with_context(malformed)?, color::profile_from(bytes)),
        Format::Heic => (decode_heic(bytes).with_context(malformed)?, color::profile_from(bytes)),
        // PNG, GIF, BMP and TIFF share one pure-Rust decoder.
        _ => (decode_via_image_crate(bytes).with_context(malformed)?, color::profile_from(bytes)),
    };

    // A format that turns its own pixels the right way up has already done so;
    // applying the EXIF tag as well would rotate the image twice.
    let orientation = if format.orients_itself() {
        Orientation::Normal
    } else {
        read_orientation(bytes)
    };

    Ok(LoadedImage {
        image,
        orientation,
        fidelity: Fidelity::Full,
        profile,
        // A raster format is its pixels; there is nothing to redraw from.
        vector: None,
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
        // The orientation tag lives in the primary IFD and applies to both the
        // full image and its thumbnail, so the quick frame is not shown
        // sideways for the moment before the real one replaces it.
        orientation: orientation_from(&exif),
        fidelity: Fidelity::Thumbnail,
        // The profile describes the file, so it covers the thumbnail too: the
        // quick frame and the image replacing it are the same colour.
        profile: color::profile_from(bytes),
        // Thumbnails come from EXIF, which only raster formats carry.
        vector: None,
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

    Ok(DecodedImage { width, height, pixels })
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
    })
}

/// WebP goes through `image-webp` directly rather than through the `image`
/// crate wrapping it, because only the crate itself exposes the ICC profile —
/// and an untagged wide-gamut WebP shown as sRGB is exactly the silent colour
/// error this viewer exists to avoid.
fn decode_webp(bytes: &[u8]) -> Result<DecodedImage> {
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes)).context("the WebP header is unreadable")?;
    let (width, height) = decoder.dimensions();

    let expected = pixel_count(width, height)?;
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

    Ok(DecodedImage { width, height, pixels })
}

/// Decode a JPEG XL, returning the pixels together with the profile the file
/// carries.
///
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
    let expected = pixel_count(width, height)?;

    // 10- and 12-bit sources come back as 16-bit samples and are narrowed
    // here, because the renderer uploads RGBA8. The depth is not thrown away
    // silently for ever: HDR (v0.10.0) is where it starts to matter, and this
    // is one of the places that will read the wider buffer instead.
    let pixels = image.to_rgba8();
    if pixels.len() != expected {
        bail!("the HEIC decoded to {} bytes but {width}x{height} RGBA needs {expected}", pixels.len());
    }

    Ok(DecodedImage { width, height, pixels })
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

    let expected = pixel_count(width, height)?;
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
        2 => grey_to_rgba8(samples.chunks_exact(2).map(|pair| (pair[0], pair[1])), expected),
        1 => grey_to_rgba8(samples.iter().map(|grey| (*grey, 0xFF)), expected),
        // CMYK reaches the same stream with five channels or more. Left
        // unhandled rather than guessed at: converting it needs the profile,
        // and no such file has been seen in the wild here.
        other => bail!("a JPEG XL with {other} channels per pixel is not one this build can show"),
    };

    Ok((DecodedImage { width, height, pixels }, profile))
}

/// Bytes needed for a `width` by `height` RGBA8 image.
///
/// Checked rather than multiplied: the dimensions come from a file that may be
/// lying, and a 32-bit overflow here would size the buffer far too small.
fn pixel_count(width: u32, height: u32) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
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
    for (target, (grey, alpha)) in pixels.chunks_exact_mut(4).zip(samples) {
        target[..3].fill(grey);
        target[3] = alpha;
    }
    pixels
}

/// Widen opaque RGB samples to the RGBA8 the renderer uploads.
fn rgb_to_rgba8(rgb: &[u8], expected: usize) -> Vec<u8> {
    let mut pixels = vec![0xFFu8; expected];
    for (target, source) in pixels.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
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
        for pixel in loaded.image.pixels.chunks_exact(4) {
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
        for pixel in loaded.image.pixels.chunks_exact(4) {
            assert_eq!(pixel, [10, 200, 90, 0xFF], "a lossless JPEG XL came back with different pixels");
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
        for (pixel, grey) in loaded.image.pixels.chunks_exact(4).zip(&pixels) {
            assert_eq!(pixel, [*grey, *grey, *grey, 0xFF], "a grey sample was not spread across the colour channels");
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
            loaded.image.pixels.chunks_exact(4).all(|pixel| pixel[3] == 0xFF),
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

    /// HEIC pixels arrive already converted to sRGB, so the profile must not
    /// be attached — applying it would convert them a second time, see
    /// ADR 0007.
    ///
    /// Asserted on a file that really does carry an ICC profile, so this
    /// cannot pass merely because there was nothing to find.
    #[test]
    fn a_tagged_heic_arrives_without_a_profile_to_apply() {
        // The fixture carries a profile: a format that read it would find one.
        let tagged = from_base64(HEIC_GRADIENT_TAGGED);
        assert!(
            tagged.windows(4).any(|window| window == b"prof"),
            "the fixture is meant to carry an ICC profile"
        );

        assert!(decode(&tagged).unwrap().profile.is_none());
        assert!(decode(&from_base64(HEIC_GRADIENT)).unwrap().profile.is_none());
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
        assert!(pixel_count(u32::MAX, u32::MAX).is_err());
        assert!(pixel_count(0, 0).is_err());
        assert_eq!(pixel_count(2, 3).unwrap(), 24);
    }

    #[test]
    fn orientation_swaps_axes_only_when_rotated_by_a_quarter_turn() {
        assert!(!Orientation::Normal.swaps_axes());
        assert!(!Orientation::Rotate180.swaps_axes());
        assert!(Orientation::Rotate90.swaps_axes());
        assert!(Orientation::Rotate270.swaps_axes());
        assert!(Orientation::Transpose.swaps_axes());
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

    #[test]
    fn display_size_follows_orientation() {
        let loaded = LoadedImage {
            image: DecodedImage {
                width: 100,
                height: 50,
                pixels: Vec::new(),
            },
            orientation: Orientation::Rotate90,
            fidelity: Fidelity::Full,
            profile: None,
            vector: None,
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
}
