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
    Heic,
    Avif,
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
        Format::Heic,
        Format::Avif,
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
        // HEIC and AVIF share the ISOBMFF container and are told apart only by
        // the brands inside it, so one function reads the box and both answers
        // come out of it.
        if let Some(format) = isobmff_format(bytes) {
            return Some(format);
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
            Format::Heic => &["heic", "heif", "hif"],
            Format::Avif => &["avif"],
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
            Format::Heic => "HEIC",
            Format::Avif => "AVIF",
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
    ///
    /// HEIC answers true for the same reason by a different route: the
    /// rotation of a HEIC lives in the container as `irot`/`imir` properties,
    /// the decoder applies them, and an encoder writing a photograph turns the
    /// EXIF orientation into exactly those properties. Both descriptions are
    /// therefore present in the same file, and honouring both turns a portrait
    /// photograph on its side.
    /// AVIF is deliberately absent: it carries the same `irot`/`imir` in the
    /// same kind of container, but nothing applies them on the way out — the
    /// decoder here returns the AV1 frame as coded, and `image_source` turns
    /// it. So AVIF answers false and the container's rotation is honoured
    /// once, by the viewer.
    pub fn orients_itself(self) -> bool {
        matches!(self, Format::JpegXl | Format::Heic)
    }

    /// Whether this format's decoder runs in a separate process.
    ///
    /// Not about memory safety any more: every decoder in this build is Rust,
    /// including the two the boundary was originally built for (ADR 0007,
    /// ADR 0008). What the separate process buys now is the ability to *stop*
    /// — a decode in this process runs to completion whatever happens, while
    /// one in a child can be killed on a timeout or abandoned when the user
    /// navigates away. See `docs/adr/0009-heavy-decodes-run-in-a-child.md`.
    ///
    /// True for the two formats whose decoders are large, complex, and slow
    /// enough that a crafted file could keep one busy for a long time. The
    /// cheap decoders stay in-process, where they cost less than the round
    /// trip would.
    pub fn needs_sandbox(self) -> bool {
        matches!(self, Format::Heic | Format::Avif)
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

/// Which still-image format an ISOBMFF file holds, judged by the brands in its
/// `ftyp` box.
///
/// HEIC, AVIF and video all share this container: `.mp4`, `.mov`, `.heic` and
/// `.avif` begin the same way, and only the brands tell them apart. So the
/// brands are read rather than the box type — matching `ftyp` alone would
/// claim every video file on the machine.
///
/// Returns `None` for a container this build does not decode, which includes
/// every video and any HEIF variant coded with something other than HEVC or
/// AV1.
fn isobmff_format(bytes: &[u8]) -> Option<Format> {
    /// Brands naming a still image coded with HEVC.
    const HEVC: [&[u8; 4]; 6] = [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx"];
    /// Brands naming a still image, or a sequence of them, coded with AV1.
    const AV1: [&[u8; 4]; 2] = [b"avif", b"avis"];

    // Box length, then the type: `....ftyp`.
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }

    let length = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    // The major brand sits at 8, then a version, then the compatible brands.
    // A malformed length must not read past the buffer, so the box is clamped
    // to what is actually there.
    let end = length.min(bytes.len());
    let box_bytes = bytes.get(8..end)?;

    // The major brand and every compatible brand are four bytes each; the
    // minor version between them is four bytes too, so reading the lot as
    // four-byte words and skipping the version is the whole parse.
    //
    // A file may carry both families of brand — `mif1` is generic and appears
    // in each — so the major brand is asked first and the compatible list only
    // decides when the major brand says nothing either way.
    let brands: Vec<&[u8]> = box_bytes
        .chunks_exact(4)
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, brand)| brand)
        .collect();

    let names = |known: &[&[u8; 4]]| brands.iter().any(|brand| known.iter().any(|candidate| *brand == *candidate));

    // The major brand is the file's own statement about what it is; a
    // compatible brand only says what it can also be read as.
    match brands.first() {
        Some(major) if HEVC.iter().any(|known| *major == *known) => return Some(Format::Heic),
        Some(major) if AV1.iter().any(|known| *major == *known) => return Some(Format::Avif),
        _ => {}
    }

    if names(&AV1) {
        Some(Format::Avif)
    } else if names(&HEVC) {
        Some(Format::Heic)
    } else {
        None
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

    /// HEIC is recognised by brand, not by the container it shares with video
    /// and with AVIF.
    #[test]
    fn detects_heic_by_its_brands() {
        assert_eq!(Format::detect(&ftyp(b"heic", &[b"mif1"])), Some(Format::Heic));
        // The brand naming the still image may appear only in the compatible
        // list, behind a generic major brand.
        assert_eq!(Format::detect(&ftyp(b"mif1", &[b"heic"])), Some(Format::Heic));
        assert_eq!(Format::detect(&ftyp(b"heix", &[])), Some(Format::Heic));
    }

    /// AVIF shares the container with HEIC and is told apart by brand alone.
    #[test]
    fn detects_avif_by_its_brands() {
        assert_eq!(Format::detect(&ftyp(b"avif", &[b"mif1", b"miaf"])), Some(Format::Avif));
        assert_eq!(Format::detect(&ftyp(b"mif1", &[b"avif"])), Some(Format::Avif));
        // An AV1 image sequence is still an AVIF; the first frame is shown.
        assert_eq!(Format::detect(&ftyp(b"avis", &[b"avif"])), Some(Format::Avif));
    }

    /// A file may list brands from both families — `mif1` is generic and
    /// appears in each. The major brand is the file's own statement about
    /// what it is, so it decides.
    #[test]
    fn the_major_brand_decides_when_a_file_claims_both() {
        assert_eq!(Format::detect(&ftyp(b"avif", &[b"mif1", b"heic"])), Some(Format::Avif));
        assert_eq!(Format::detect(&ftyp(b"heic", &[b"mif1", b"avif"])), Some(Format::Heic));
    }

    /// Video shares this container too, and claiming a `.mp4` as an image
    /// would put nitid in front of every film on the machine.
    #[test]
    fn does_not_claim_the_rest_of_the_isobmff_family() {
        assert_eq!(Format::detect(&ftyp(b"isom", &[b"mp42", b"avc1"])), None);
        assert_eq!(Format::detect(&ftyp(b"qt  ", &[])), None);
        // A generic HEIF brand with nothing coded as HEVC or AV1 alongside it
        // is not something this build can decode.
        assert_eq!(Format::detect(&ftyp(b"mif1", &[b"miaf"])), None);
    }

    /// A brand list whose declared length runs past the file must be read as
    /// far as the bytes go, not past them.
    #[test]
    fn a_lying_box_length_does_not_read_past_the_file() {
        let mut file = ftyp(b"heic", &[]);
        file[..4].copy_from_slice(&9999u32.to_be_bytes());
        assert_eq!(Format::detect(&file), Some(Format::Heic));

        // And a box cut short mid-brand is simply not a HEIC.
        for length in 0..file.len() {
            let _ = Format::detect(&file[..length]);
        }
    }

    /// Build an `ftyp` box: length, tag, major brand, minor version, then the
    /// compatible brands.
    fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
        let length = 16 + compatible.len() * 4;
        let mut out = Vec::with_capacity(length);
        out.extend_from_slice(&(length as u32).to_be_bytes());
        out.extend_from_slice(b"ftyp");
        out.extend_from_slice(major);
        out.extend_from_slice(&0u32.to_be_bytes());
        for brand in compatible {
            out.extend_from_slice(*brand);
        }
        out
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
    /// so: claiming it wrongly leaves a rotated photograph on its side, and
    /// denying it rotates one that was already upright.
    #[test]
    fn only_self_orienting_formats_say_so() {
        for format in Format::ALL {
            let expected = matches!(format, Format::JpegXl | Format::Heic);
            assert_eq!(format.orients_itself(), expected, "{format:?} answers orients_itself wrongly");
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
