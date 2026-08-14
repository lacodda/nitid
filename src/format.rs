//! Recognising an image format from the bytes that begin the file.
//!
//! The name on disk is a hint, not a fact: a `.png` that is really a JPEG is
//! common enough that trusting the extension would refuse a file the user can
//! plainly see open elsewhere. So the extension only decides what appears in a
//! folder listing, and the content decides what actually decodes it.
//!
//! This is the one place a format is named. A new decoder adds a variant here,
//! its signature to `Format::detect`, and its extensions to `EXTENSIONS`; the
//! decoder, the file associations `nitid install` registers, and the ICC lookup
//! all follow from that single entry.

/// An image format this build knows how to open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Jpeg,
    Png,
    WebP,
    JpegXl,
    Svg,
    Gif,
    Bmp,
    Tiff,
}

impl Format {
    /// Every format, in the order they appear above.
    ///
    /// Exists so the extension list and the tests cannot silently fall behind a
    /// newly added variant.
    pub const ALL: &'static [Format] = &[
        Format::Jpeg,
        Format::Png,
        Format::WebP,
        Format::JpegXl,
        Format::Svg,
        Format::Gif,
        Format::Bmp,
        Format::Tiff,
    ];

    /// Identify the format from the start of the file.
    ///
    /// Returns `None` for anything unrecognised, which the caller reports as a
    /// file nitid cannot open.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return Some(Format::Jpeg);
        }
        if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            return Some(Format::Png);
        }
        // RIFF....WEBP: the size sits between the two tags, so both halves are
        // checked and the four bytes carrying the length are skipped.
        if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
            return Some(Format::WebP);
        }
        // JPEG XL is stored two ways: a bare codestream, and the same
        // codestream wrapped in an ISOBMFF container. Both are ordinary `.jxl`
        // files in the wild, so both signatures are recognised.
        if bytes.starts_with(&[0xFF, 0x0A]) || bytes.starts_with(&[0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', 0x20, 0x0D, 0x0A, 0x87, 0x0A]) {
            return Some(Format::JpegXl);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(Format::Gif);
        }
        if bytes.starts_with(b"BM") {
            return Some(Format::Bmp);
        }
        // TIFF is either endianness, each with its own magic number.
        if bytes.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || bytes.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
            return Some(Format::Tiff);
        }
        // SVG is the one format here with no signature to match: it is XML, and
        // the `<svg` element may sit behind a declaration, a doctype, comments
        // or whitespace. So the opening of the file is searched for the tag
        // instead — bounded, because scanning a large binary for text would
        // cost more than every other check combined.
        if looks_like_svg(bytes) {
            return Some(Format::Svg);
        }

        None
    }

    /// The extensions this format is normally stored under, lowercase and
    /// without the dot.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Jpeg => &["jpg", "jpeg", "jpe", "jfif"],
            Format::Png => &["png"],
            Format::WebP => &["webp"],
            Format::JpegXl => &["jxl"],
            Format::Svg => &["svg"],
            Format::Gif => &["gif"],
            Format::Bmp => &["bmp"],
            Format::Tiff => &["tif", "tiff"],
        }
    }

    /// The name shown to a person, as the format is usually written.
    pub fn name(self) -> &'static str {
        match self {
            Format::Jpeg => "JPEG",
            Format::Png => "PNG",
            Format::WebP => "WebP",
            Format::JpegXl => "JPEG XL",
            Format::Svg => "SVG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::Tiff => "TIFF",
        }
    }

    /// Whether the decoder returns pixels already turned the right way up.
    ///
    /// Most formats store the orientation as EXIF metadata and hand back the
    /// pixels as encoded, leaving the rotation to the viewer. JPEG XL carries
    /// orientation in its own header, and `jxl-oxide` applies it while
    /// rendering — both to the pixels and to the dimensions it reports.
    ///
    /// Applying the EXIF tag on top of that would rotate such an image twice,
    /// so the viewer asks here instead of assuming.
    pub fn orients_itself(self) -> bool {
        matches!(self, Format::JpegXl)
    }

    /// Whether this format's decoder must run in a sandboxed process.
    ///
    /// True for formats decoded by a C library, where a malformed file is a
    /// memory-safety bug rather than a panic — see
    /// `docs/adr/0002-sandbox-c-decoders.md`. Nothing answers true yet: every
    /// decoder in this build is pure Rust, and HEIC and AVIF arrive in v0.7.0.
    /// The boundary is built first so that adding them is adding a decoder,
    /// not adding a decoder and an architecture at once.
    pub fn needs_sandbox(self) -> bool {
        false
    }

    /// Whether the file describes shapes rather than a grid of pixels.
    ///
    /// A vector image has no resolution of its own: it is rasterised for the
    /// size it is shown at, and rasterised again when that size changes. The
    /// viewer keeps the source for such a format instead of treating the first
    /// rasterisation as the image itself.
    pub fn is_vector(self) -> bool {
        matches!(self, Format::Svg)
    }
}

/// Whether the start of the file reads as an SVG document.
///
/// SVG is the one format here with nothing to match at a fixed offset — it is
/// XML, and the root element may sit behind a declaration, a doctype, comments
/// or blank lines. Only the opening of the file is searched, and only for the
/// element name: a binary that happens to contain `<svg` a megabyte in is not
/// an SVG, and reading that far to find out would cost more than every other
/// signature check combined.
fn looks_like_svg(bytes: &[u8]) -> bool {
    /// Room for an XML declaration, a doctype and a comment or two ahead of the
    /// root element — as much preamble as files in the wild carry.
    const SEARCH_LIMIT: usize = 1024;

    let head = &bytes[..bytes.len().min(SEARCH_LIMIT)];
    // Matched case-insensitively: XML element names are case-sensitive, but
    // `<SVG` appears in the wild and refusing to open it helps nobody.
    head.windows(4).any(|window| window.eq_ignore_ascii_case(b"<svg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_format_from_its_signature() {
        assert_eq!(Format::detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(Format::detect(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]), Some(Format::Png));
        assert_eq!(Format::detect(b"RIFF\0\0\0\0WEBPVP8 "), Some(Format::WebP));
        // JPEG XL, bare codestream and ISOBMFF container alike.
        assert_eq!(Format::detect(&[0xFF, 0x0A, 0x00]), Some(Format::JpegXl));
        assert_eq!(
            Format::detect(&[0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', 0x20, 0x0D, 0x0A, 0x87, 0x0A]),
            Some(Format::JpegXl)
        );
        assert_eq!(Format::detect(b"GIF89a..."), Some(Format::Gif));
        assert_eq!(Format::detect(b"BM......"), Some(Format::Bmp));
        assert_eq!(Format::detect(&[0x49, 0x49, 0x2A, 0x00]), Some(Format::Tiff));
        assert_eq!(Format::detect(&[0x4D, 0x4D, 0x00, 0x2A]), Some(Format::Tiff));
    }

    #[test]
    fn rejects_what_it_does_not_know() {
        assert_eq!(Format::detect(b""), None);
        assert_eq!(Format::detect(b"this is not an image"), None);
        // RIFF alone is not enough: WAV audio starts the same way.
        assert_eq!(Format::detect(b"RIFF\0\0\0\0WAVEfmt "), None);
    }

    /// Both formats begin with 0xFF, and the byte after it is the whole
    /// difference. Reading a JPEG as JPEG XL would refuse the format the
    /// viewer opens most.
    #[test]
    fn jpeg_and_jpeg_xl_are_not_confused_for_each_other() {
        assert_eq!(Format::detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(Format::detect(&[0xFF, 0x0A, 0x00, 0x00]), Some(Format::JpegXl));
        // 0xFF followed by neither is not an image this build knows.
        assert_eq!(Format::detect(&[0xFF, 0x00, 0x00, 0x00]), None);
    }

    /// SVG has no signature at a fixed offset, so it is recognised by its root
    /// element — which real files put behind a declaration, a doctype, or
    /// nothing at all.
    #[test]
    fn detects_svg_behind_whatever_precedes_the_root_element() {
        assert_eq!(Format::detect(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), Some(Format::Svg));
        assert_eq!(Format::detect(b"<?xml version=\"1.0\"?>\n<svg/>"), Some(Format::Svg));
        assert_eq!(Format::detect(b"<!-- drawn by hand -->\n<svg/>"), Some(Format::Svg));
        // Case-insensitive: `<SVG` appears in the wild.
        assert_eq!(Format::detect(b"<SVG/>"), Some(Format::Svg));
    }

    /// The search is bounded, so a file that merely mentions the element far
    /// in is not mistaken for one — and scanning a large binary end to end
    /// never happens.
    #[test]
    fn a_mention_of_svg_past_the_opening_is_not_a_document() {
        let mut file = vec![b' '; 4096];
        file.extend_from_slice(b"<svg/>");
        assert_eq!(Format::detect(&file), None);
    }

    /// Only formats without a fixed resolution may claim to be vector: the
    /// viewer keeps the source for those and rasterises again on zoom.
    #[test]
    fn only_svg_is_a_vector_format() {
        for format in Format::ALL {
            assert_eq!(format.is_vector(), *format == Format::Svg, "{format:?} answers is_vector wrongly");
        }
    }

    /// Only the formats whose decoder applies the orientation itself may say
    /// so: claiming it wrongly leaves a rotated photograph on its side.
    #[test]
    fn only_jpeg_xl_orients_itself() {
        for format in Format::ALL {
            assert_eq!(format.orients_itself(), *format == Format::JpegXl, "{format:?} answers orients_itself wrongly");
        }
    }

    /// A truncated file must be reported as unknown rather than reaching into
    /// bytes that are not there.
    #[test]
    fn a_signature_cut_short_does_not_panic() {
        for length in 0..12 {
            let _ = Format::detect(&[0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', 0x20, 0x0D, 0x0A, 0x87, 0x0A][..length]);
            let _ = Format::detect(&b"RIFF\0\0\0\0WEBP"[..length]);
        }
    }

    #[test]
    fn every_format_has_an_extension_and_a_name() {
        for format in Format::ALL {
            assert!(!format.extensions().is_empty(), "{format:?} has no extension");
            assert!(!format.name().is_empty(), "{format:?} has no name");
        }
    }

    /// Two formats claiming the same extension would make the folder listing
    /// ambiguous and the install registration order-dependent.
    #[test]
    fn no_extension_is_claimed_twice() {
        let mut seen = Vec::new();
        for format in Format::ALL {
            for extension in format.extensions() {
                assert!(!seen.contains(extension), "{extension} is claimed twice");
                seen.push(extension);
            }
        }
    }
}
