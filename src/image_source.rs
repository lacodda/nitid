//! Decoding an image file into pixels the renderer can upload.
//!
//! Every decoder in this module is pure Rust: a malformed file costs a panic or
//! an error, never code execution. C-backed formats (HEIC, AVIF) arrive in
//! v0.7.0 behind a separate process — see `docs/adr/0002-sandbox-c-decoders.md`.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};
use moxcms::ColorProfile;

use crate::color;
use crate::format::Format;

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
    if bytes.is_empty() {
        bail!("the file is empty");
    }

    let Some(format) = Format::detect(bytes) else {
        bail!("the format is not one nitid can open");
    };

    let malformed = || format!("the {} data is malformed", format.name());

    // JPEG XL comes back with its profile attached: one pass over the file
    // yields both, where reading the profile separately — as the other formats
    // do — would mean decoding the image twice.
    let (image, profile) = match format {
        Format::JpegXl => decode_jxl(bytes).with_context(malformed)?,
        Format::Jpeg => (decode_jpeg(bytes).with_context(malformed)?, color::profile_from(bytes)),
        Format::WebP => (decode_webp(bytes).with_context(malformed)?, color::profile_from(bytes)),
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

    #[test]
    fn widening_grey_samples_fills_all_three_colour_channels() {
        assert_eq!(grey_to_rgba8([(40u8, 0xFF)].into_iter(), 4), vec![40, 40, 40, 0xFF]);
        // Grey with alpha keeps the alpha it was given.
        assert_eq!(grey_to_rgba8([(10u8, 20u8), (30, 40)].into_iter(), 8), vec![10, 10, 10, 20, 30, 30, 30, 40]);
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
