//! Decoding an image file into pixels the renderer can upload.
//!
//! Every decoder in this module is pure Rust: a malformed file costs a panic or
//! an error, never code execution. C-backed formats (HEIC, AVIF) arrive in
//! v0.5.0 behind a separate process — see `docs/adr/0002-sandbox-c-decoders.md`.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Extensions this build can open, lowercase and without the dot.
///
/// Used both to filter a folder listing and to pick a decoder.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "tif", "tiff"];

/// A decoded image: tightly packed RGBA8 rows, ready for a GPU upload.
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
pub struct LoadedImage {
    pub image: DecodedImage,
    pub orientation: Orientation,
    pub fidelity: Fidelity,
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
        .is_some_and(|ext| SUPPORTED_EXTENSIONS.contains(&ext.as_str()))
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

    let orientation = read_orientation(bytes);
    let image = if is_jpeg(bytes) {
        decode_jpeg(bytes)?
    } else {
        decode_via_image_crate(bytes)?
    };

    Ok(LoadedImage {
        image,
        orientation,
        fidelity: Fidelity::Full,
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
    })
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
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
