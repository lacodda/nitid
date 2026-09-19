//! Taking the metadata out of a file without touching its picture.
//!
//! A photograph carries more than the photograph. EXIF names the camera and
//! often the place; XMP carries whatever the editing program felt like
//! writing, including the owner's name; IPTC carries a copyright and a
//! caption. Sending a picture to someone sends all of it, and the person
//! sending it usually does not know that.
//!
//! So this is not an export. **The compressed picture is not read, not decoded
//! and not re-encoded** — the file is walked at the level of its container and
//! the parts that describe rather than depict are left out of the copy. A
//! scrubbed JPEG is byte-identical from its start-of-scan marker onward, which
//! the tests assert on the bytes. The same promise ADR 0024 makes about a
//! saved turn, for the same reason: a viewer that stripped a photograph's EXIF
//! by re-encoding it would have traded one loss the owner can see for one they
//! cannot.
//!
//! What is *not* metadata, and stays: the colour profile. A picture whose
//! profile is removed does not become anonymous, it becomes wrong — its
//! numbers now claim to be sRGB when they are not, and every program that
//! reads them will be wrong in the same direction. The profile says nothing
//! about the owner, the camera or the place. [`Keep::Profile`] is the default
//! and [`Keep::Nothing`] is there for the person who has already baked the
//! colour in (`Ctrl+Shift+S`) and wants the file to state nothing at all.
//!
//! The orientation is the one field that is metadata and also *load-bearing*:
//! remove it and a photograph taken sideways is shown sideways. It is handled
//! separately, by [`crate::bake`], which turns it into pixels so that removing
//! the tag costs nothing.

use crate::format::Format;

/// What a scrub is allowed to leave in the file.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Keep {
    /// Strip what describes the owner, the camera and the place; keep the
    /// colour profile, which describes the picture.
    ///
    /// The default: this is what "send this without telling them where I live"
    /// means, and it is the only one of the two that cannot make the picture
    /// look wrong somewhere else.
    #[default]
    Profile,
    /// Strip everything, the colour profile included.
    ///
    /// For a picture whose colour has already been baked into its numbers, or
    /// one going somewhere that ignores profiles anyway. The numbers are then
    /// read as sRGB by whoever receives them, which is right only if that is
    /// what they are.
    Nothing,
}

impl Keep {
    /// What this is called in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Keep::Profile => "Keep the colour profile",
            Keep::Nothing => "Strip everything, profile included",
        }
    }
}

/// What a file was found to be carrying, before anything is removed.
///
/// Counted so the interface can say what will go rather than asking a person
/// to trust that something did. A file with nothing to strip is told so
/// instead of being rewritten for no reason.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Found {
    /// EXIF: camera, exposure, date, and the GPS fix if the camera had one.
    pub exif: bool,
    /// XMP: whatever the editing program wrote, often including a name.
    pub xmp: bool,
    /// IPTC: caption, copyright, keywords.
    pub iptc: bool,
    /// A colour profile. Not stripped by default, and listed so the interface
    /// can say that it is staying.
    pub profile: bool,
    /// Bytes of the file that are metadata rather than picture.
    pub bytes: usize,
}

impl Found {
    /// Whether there is anything here a scrub would remove under `keep`.
    pub fn anything(self, keep: Keep) -> bool {
        self.exif || self.xmp || self.iptc || (keep == Keep::Nothing && self.profile)
    }

    /// What is in the file, in the words the interface says.
    ///
    /// Named rather than counted: "EXIF, XMP" tells a person what they are
    /// about to lose, and "3 blocks" does not.
    pub fn describe(self) -> String {
        let mut parts = Vec::new();
        if self.exif {
            parts.push("EXIF");
        }
        if self.xmp {
            parts.push("XMP");
        }
        if self.iptc {
            parts.push("IPTC");
        }
        if parts.is_empty() {
            return "nothing to strip".to_string();
        }
        format!("{} ({})", parts.join(", "), readable(self.bytes))
    }
}

/// A size as a person reads it.
fn readable(bytes: usize) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kilobytes = bytes as f64 / 1024.0;
    if kilobytes < 1024.0 {
        return format!("{kilobytes:.1} kB");
    }
    format!("{:.1} MB", kilobytes / 1024.0)
}

/// Why a file cannot be scrubbed this way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The format has no container this module can walk without a decoder.
    ///
    /// JPEG, PNG and WebP are segment or chunk streams whose metadata can be
    /// dropped by skipping it. HEIC and AVIF keep theirs in a box tree whose
    /// offsets point into the picture data, so removing a box means rewriting
    /// those offsets — a larger job, and one whose failure mode is a file that
    /// no longer decodes. JPEG XL and SVG are their own shapes again.
    Unsupported,
    /// The file's structure did not hold together while walking it.
    Malformed,
}

impl Refusal {
    /// What to say to a person about it.
    pub fn reason(self) -> &'static str {
        match self {
            Refusal::Unsupported => "this format keeps its metadata somewhere that cannot be edited without re-encoding the picture",
            Refusal::Malformed => "this file's structure could not be read to the end",
        }
    }
}

/// Which formats can be scrubbed at all.
///
/// Asked before the work, so the interface can grey the action out rather than
/// offering something that will refuse.
pub fn supported(format: Format) -> bool {
    matches!(format, Format::Jpeg | Format::Png | Format::WebP)
}

/// What `bytes` is carrying.
pub fn survey(bytes: &[u8], format: Format) -> Result<Found, Refusal> {
    match format {
        Format::Jpeg => jpeg::survey(bytes),
        Format::Png => png::survey(bytes),
        Format::WebP => webp::survey(bytes),
        _ => Err(Refusal::Unsupported),
    }
}

/// The same file with its metadata left out.
///
/// The picture's own bytes are copied across untouched. Returns the new file,
/// which is always smaller than the original or the same size.
pub fn scrub(bytes: &[u8], format: Format, keep: Keep) -> Result<Vec<u8>, Refusal> {
    match format {
        Format::Jpeg => jpeg::scrub(bytes, keep),
        Format::Png => png::scrub(bytes, keep),
        Format::WebP => webp::scrub(bytes, keep),
        _ => Err(Refusal::Unsupported),
    }
}

// ---------------------------------------------------------------------------
// JPEG: a stream of marker segments
// ---------------------------------------------------------------------------

/// What one APP segment of a JPEG turns out to be.
///
/// The marker alone does not say: APP1 is EXIF *or* XMP depending on the
/// string that follows it, and APP2 is the colour profile *or* something a
/// camera maker invented. So the payload's first bytes decide, and a segment
/// whose signature is not recognised is left alone — a scrub that dropped
/// every APP segment it did not know would be removing parts of files it
/// cannot describe.
mod jpeg {
    use super::{Found, Keep, Refusal};

    /// What a JPEG segment is, once its payload has been looked at.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Kind {
        Exif,
        Xmp,
        Iptc,
        Profile,
        /// A comment. Usually an encoder's name, sometimes a person's note —
        /// either way it is text about the file rather than the picture.
        Comment,
        /// Something else: the picture, the tables, a maker note this module
        /// does not claim to understand.
        Other,
    }

    /// Classify the segment starting at `marker` with payload `payload`.
    pub(super) fn classify(marker: u8, payload: &[u8]) -> Kind {
        match marker {
            // APP1 carries EXIF and XMP, told apart by the signature.
            0xE1 => {
                if payload.starts_with(b"Exif\0\0") {
                    Kind::Exif
                } else if payload.starts_with(b"http://ns.adobe.com/xap/1.0/\0") {
                    Kind::Xmp
                } else {
                    Kind::Other
                }
            }
            // APP2 carries the ICC profile, in numbered chunks.
            0xE2 => {
                if payload.starts_with(b"ICC_PROFILE\0") {
                    Kind::Profile
                } else {
                    Kind::Other
                }
            }
            // APP13 carries Photoshop's resource block, which is where IPTC
            // lives.
            0xED => {
                if payload.starts_with(b"Photoshop 3.0\0") {
                    Kind::Iptc
                } else {
                    Kind::Other
                }
            }
            // The comment marker.
            0xFE => Kind::Comment,
            _ => Kind::Other,
        }
    }

    /// Whether a scrub under `keep` drops this kind.
    pub(super) fn dropped(kind: Kind, keep: Keep) -> bool {
        match kind {
            Kind::Exif | Kind::Xmp | Kind::Iptc | Kind::Comment => true,
            Kind::Profile => keep == Keep::Nothing,
            Kind::Other => false,
        }
    }

    pub(super) fn survey(bytes: &[u8]) -> Result<Found, Refusal> {
        let mut found = Found::default();
        walk(bytes, |kind, segment| {
            match kind {
                Kind::Exif => found.exif = true,
                Kind::Xmp => found.xmp = true,
                Kind::Iptc => found.iptc = true,
                Kind::Profile => found.profile = true,
                Kind::Comment | Kind::Other => {}
            }
            // Counted the way the file counts it: the whole segment, marker
            // and length included, because that is what the copy will be
            // smaller by.
            if matches!(kind, Kind::Exif | Kind::Xmp | Kind::Iptc | Kind::Comment) {
                found.bytes += segment.len();
            }
        })?;
        Ok(found)
    }

    pub(super) fn scrub(bytes: &[u8], keep: Keep) -> Result<Vec<u8>, Refusal> {
        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&[0xFF, 0xD8]);
        let tail = walk(bytes, |kind, segment| {
            if !dropped(kind, keep) {
                out.extend_from_slice(segment);
            }
        })?;

        // Everything from the scan onward is the picture: copied as one block,
        // which is what makes this lossless in the strong sense. No encoder
        // runs, so there is nothing for an encoder to get wrong.
        out.extend_from_slice(&bytes[tail..]);
        Ok(out)
    }

    /// Walk the segments between the start-of-image marker and the scan,
    /// handing each to `visit`, and return where the picture begins.
    ///
    /// The walk deliberately stops at the scan rather than trying to parse
    /// past it: entropy-coded data contains byte pairs that look like markers,
    /// and a walk that kept going would find segments that are not there. A
    /// JPEG's metadata is always before the scan.
    fn walk(bytes: &[u8], mut visit: impl FnMut(Kind, &[u8])) -> Result<usize, Refusal> {
        if !bytes.starts_with(&[0xFF, 0xD8]) {
            return Err(Refusal::Malformed);
        }

        let mut at = 2usize;
        loop {
            // Padding: a run of 0xFF bytes before a marker is legal.
            while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
                at += 1;
            }
            let Some(&0xFF) = bytes.get(at) else {
                return Err(Refusal::Malformed);
            };
            let Some(&marker) = bytes.get(at + 1) else {
                return Err(Refusal::Malformed);
            };

            // Markers that stand alone, with no length after them.
            if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                visit(Kind::Other, &bytes[at..at + 2]);
                at += 2;
                continue;
            }

            // The scan: the picture starts at its header and runs to the end.
            if marker == 0xDA {
                return Ok(at);
            }
            // End of image with no scan at all — a file with no picture in it.
            if marker == 0xD9 {
                return Ok(at);
            }

            let Some(length) = bytes.get(at + 2..at + 4).map(|pair| usize::from(u16::from_be_bytes([pair[0], pair[1]]))) else {
                return Err(Refusal::Malformed);
            };
            if length < 2 {
                return Err(Refusal::Malformed);
            }
            let end = at + 2 + length;
            if end > bytes.len() {
                return Err(Refusal::Malformed);
            }

            let payload = &bytes[at + 4..end];
            visit(classify(marker, payload), &bytes[at..end]);
            at = end;
        }
    }
}

// ---------------------------------------------------------------------------
// PNG: a stream of length-prefixed chunks
// ---------------------------------------------------------------------------

/// PNG is the easy one: every chunk states its own length and carries its own
/// checksum, so dropping one is removing a slice and nothing else needs
/// recomputing. The picture lives in `IDAT` chunks, which are copied across
/// untouched.
mod png {
    use super::{Found, Keep, Refusal};

    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// What one chunk is.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Kind {
        Exif,
        Xmp,
        Iptc,
        Profile,
        /// A text chunk: `tEXt`, `zTXt`, `iTXt`. Comments, software names, and
        /// whatever else was written in.
        Text,
        /// The time the file was last changed. A date is metadata about the
        /// person's day, not about the picture.
        Time,
        Other,
    }

    /// Classify a chunk by its four-letter type, and by the keyword inside it
    /// where the type alone does not say.
    pub(super) fn classify(kind: &[u8; 4], body: &[u8]) -> Kind {
        match kind {
            b"eXIf" => Kind::Exif,
            b"iCCP" => Kind::Profile,
            b"tIME" => Kind::Time,
            // XMP arrives as an `iTXt` chunk with a fixed keyword; everything
            // else in an `iTXt` is ordinary text.
            b"iTXt" => {
                if body.starts_with(b"XML:com.adobe.xmp\0") {
                    Kind::Xmp
                } else {
                    Kind::Text
                }
            }
            b"tEXt" | b"zTXt" => {
                // Photoshop writes IPTC into a `tEXt` chunk under this
                // keyword; other keywords are plain text either way.
                if body.starts_with(b"Raw profile type iptc\0") {
                    Kind::Iptc
                } else {
                    Kind::Text
                }
            }
            _ => Kind::Other,
        }
    }

    pub(super) fn dropped(kind: Kind, keep: Keep) -> bool {
        match kind {
            Kind::Exif | Kind::Xmp | Kind::Iptc | Kind::Text | Kind::Time => true,
            Kind::Profile => keep == Keep::Nothing,
            Kind::Other => false,
        }
    }

    pub(super) fn survey(bytes: &[u8]) -> Result<Found, Refusal> {
        let mut found = Found::default();
        walk(bytes, |kind, chunk| {
            match kind {
                Kind::Exif => found.exif = true,
                Kind::Xmp => found.xmp = true,
                Kind::Iptc => found.iptc = true,
                Kind::Profile => found.profile = true,
                Kind::Text | Kind::Time | Kind::Other => {}
            }
            if matches!(kind, Kind::Exif | Kind::Xmp | Kind::Iptc | Kind::Text | Kind::Time) {
                found.bytes += chunk.len();
            }
        })?;
        Ok(found)
    }

    pub(super) fn scrub(bytes: &[u8], keep: Keep) -> Result<Vec<u8>, Refusal> {
        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&SIGNATURE);
        walk(bytes, |kind, chunk| {
            if !dropped(kind, keep) {
                out.extend_from_slice(chunk);
            }
        })?;
        Ok(out)
    }

    /// Walk every chunk after the signature, handing each to `visit`.
    fn walk(bytes: &[u8], mut visit: impl FnMut(Kind, &[u8])) -> Result<(), Refusal> {
        if !bytes.starts_with(&SIGNATURE) {
            return Err(Refusal::Malformed);
        }

        let mut at = SIGNATURE.len();
        while at < bytes.len() {
            let Some(header) = bytes.get(at..at + 8) else {
                return Err(Refusal::Malformed);
            };
            let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
            let kind: [u8; 4] = [header[4], header[5], header[6], header[7]];

            // Length, type, body, checksum.
            let end = at.checked_add(12).and_then(|base| base.checked_add(length)).ok_or(Refusal::Malformed)?;
            if end > bytes.len() {
                return Err(Refusal::Malformed);
            }
            let body = &bytes[at + 8..at + 8 + length];

            visit(classify(&kind, body), &bytes[at..end]);

            if &kind == b"IEND" {
                return Ok(());
            }
            at = end;
        }

        // A PNG that stops before its end marker is one this cannot rebuild
        // honestly.
        Err(Refusal::Malformed)
    }
}

// ---------------------------------------------------------------------------
// WebP: RIFF chunks, with a size in the header that has to be corrected
// ---------------------------------------------------------------------------

/// WebP is RIFF: the same length-prefixed chunks as PNG, with two differences
/// that matter here. Chunks are padded to an even length, and the file's own
/// header states the total size — so dropping a chunk means rewriting that
/// number, which is the one place this module computes rather than copies.
///
/// A file with no `VP8X` extended header carries no metadata at all, and one
/// whose only reason for having `VP8X` was the metadata keeps it: the flags
/// are corrected rather than the header removed, because removing it would
/// mean rewriting how the picture chunk is found.
mod webp {
    use super::{Found, Keep, Refusal};

    /// What one RIFF chunk is.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub(super) enum Kind {
        Exif,
        Xmp,
        Profile,
        /// The extended header, whose flag bits say which of the above are
        /// present. Kept, with its flags corrected.
        Flags,
        Other,
    }

    pub(super) fn classify(kind: &[u8; 4]) -> Kind {
        match kind {
            b"EXIF" => Kind::Exif,
            b"XMP " => Kind::Xmp,
            b"ICCP" => Kind::Profile,
            b"VP8X" => Kind::Flags,
            _ => Kind::Other,
        }
    }

    pub(super) fn dropped(kind: Kind, keep: Keep) -> bool {
        match kind {
            Kind::Exif | Kind::Xmp => true,
            Kind::Profile => keep == Keep::Nothing,
            Kind::Flags | Kind::Other => false,
        }
    }

    /// The bits of `VP8X`'s first byte that say what the file carries.
    const ICC_FLAG: u8 = 0b0010_0000;
    const EXIF_FLAG: u8 = 0b0000_1000;
    const XMP_FLAG: u8 = 0b0000_0100;

    pub(super) fn survey(bytes: &[u8]) -> Result<Found, Refusal> {
        let mut found = Found::default();
        walk(bytes, |kind, chunk| {
            match kind {
                Kind::Exif => found.exif = true,
                Kind::Xmp => found.xmp = true,
                Kind::Profile => found.profile = true,
                Kind::Flags | Kind::Other => {}
            }
            if matches!(kind, Kind::Exif | Kind::Xmp) {
                found.bytes += chunk.len();
            }
        })?;
        // WebP has no IPTC chunk at all: the format never defined one, so a
        // WebP is never carrying it.
        Ok(found)
    }

    pub(super) fn scrub(bytes: &[u8], keep: Keep) -> Result<Vec<u8>, Refusal> {
        let mut body = Vec::with_capacity(bytes.len());
        walk(bytes, |kind, chunk| {
            if dropped(kind, keep) {
                return;
            }
            if kind == Kind::Flags {
                // The header stays, and its flags are corrected to describe
                // the file that is being written rather than the one that was
                // read. A flag left set for a chunk that is gone makes a
                // decoder look for something that is not there.
                let mut corrected = chunk.to_vec();
                if let Some(flags) = corrected.get_mut(8) {
                    *flags &= !(EXIF_FLAG | XMP_FLAG);
                    if keep == Keep::Nothing {
                        *flags &= !ICC_FLAG;
                    }
                }
                body.extend_from_slice(&corrected);
                return;
            }
            body.extend_from_slice(chunk);
        })?;

        // RIFF, the size of everything after this field, then the body — which
        // already begins with the `WEBP` tag, because the walk starts after
        // the size.
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Walk the `WEBP` tag and every chunk after it.
    ///
    /// The tag is handed to `visit` as an ordinary kept block so that the
    /// scrub's body starts with it without a second special case.
    fn walk(bytes: &[u8], mut visit: impl FnMut(Kind, &[u8])) -> Result<(), Refusal> {
        if !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
            return Err(Refusal::Malformed);
        }

        // The `WEBP` tag itself, which every copy keeps.
        visit(Kind::Other, &bytes[8..12]);

        let mut at = 12usize;
        while at + 8 <= bytes.len() {
            let header = &bytes[at..at + 8];
            let kind: [u8; 4] = [header[0], header[1], header[2], header[3]];
            let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;

            // RIFF pads an odd-length chunk with one byte, which belongs to
            // the chunk as far as copying goes.
            let padded = length + (length & 1);
            let end = at.checked_add(8).and_then(|base| base.checked_add(padded)).ok_or(Refusal::Malformed)?;
            if end > bytes.len() {
                // A final chunk whose padding byte was never written is a file
                // in the wild, not a broken one: take what is there.
                let end = bytes.len();
                visit(classify(&kind), &bytes[at..end]);
                return Ok(());
            }

            visit(classify(&kind), &bytes[at..end]);
            at = end;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Fixtures
    //
    // Built by hand rather than taken from a library, for the reason the crop
    // module records: a fixture from a writer that does not emit the segment
    // under test makes a green test that never ran the code. Here the whole
    // point is the segments, so the fixtures state them byte for byte.
    // -----------------------------------------------------------------------

    /// A JPEG segment: marker, length, payload.
    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0xFF, marker];
        out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// A JPEG with the given segments before a scan, and a recognisable body.
    fn jpeg(segments: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        for part in segments {
            out.extend_from_slice(part);
        }
        // A start-of-frame, so the file has a shape a decoder would accept.
        out.extend_from_slice(&segment(0xC0, &[8, 0, 16, 0, 16, 1, 1, 0x11, 0]));
        // The scan header, then bytes standing in for entropy-coded data.
        //
        // Two traps in here on purpose, because a walk that carried on past the
        // scan would fall into both:
        //
        // - `FF 00` is byte stuffing, which is how a real JPEG writes a literal
        //   `FF` inside the picture.
        // - `FF E1` is the APP1 marker, *unstuffed*. A restart marker sequence
        //   puts real `FF Dn` pairs in the stream, so a bare `FF` followed by
        //   something is not hypothetical — and a reader that looked for
        //   segments here would find one and take a bite out of the picture.
        out.extend_from_slice(&segment(0xDA, &[1, 1, 0, 0, 63, 0]));
        out.extend_from_slice(&[0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF, 0xE1, 0x78]);
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    fn exif_segment() -> Vec<u8> {
        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(b"II\x2a\x00\x08\x00\x00\x00");
        payload.extend_from_slice(&[0; 64]);
        segment(0xE1, &payload)
    }

    fn xmp_segment() -> Vec<u8> {
        let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        payload.extend_from_slice(br#"<x:xmpmeta><rdf:RDF>owner</rdf:RDF></x:xmpmeta>"#);
        segment(0xE1, &payload)
    }

    fn iptc_segment() -> Vec<u8> {
        let mut payload = b"Photoshop 3.0\0".to_vec();
        payload.extend_from_slice(b"8BIM\x04\x04\0\0\0\0\0\x10");
        payload.extend_from_slice(&[0; 16]);
        segment(0xED, &payload)
    }

    fn profile_segment() -> Vec<u8> {
        let mut payload = b"ICC_PROFILE\0".to_vec();
        payload.push(1); // chunk one
        payload.push(1); // of one
        payload.extend_from_slice(&[0; 128]);
        segment(0xE2, &payload)
    }

    /// One PNG chunk, with the checksum the format requires.
    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = (body.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        // The checksum covers the type and the body. Computed rather than
        // faked, so that a real decoder would accept what the tests build and
        // what the scrub writes.
        let mut checked = kind.to_vec();
        checked.extend_from_slice(body);
        out.extend_from_slice(&crc32(&checked).to_be_bytes());
        out
    }

    /// The CRC-32 PNG specifies, written out rather than depended on: one
    /// polynomial and eight rounds per byte is smaller than a crate.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    fn png(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut header = Vec::new();
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&1u32.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        out.extend_from_slice(&chunk(b"IHDR", &header));
        for part in chunks {
            out.extend_from_slice(part);
        }
        out.extend_from_slice(&chunk(b"IDAT", &[0x78, 0x01, 0x01, 0x00]));
        out.extend_from_slice(&chunk(b"IEND", &[]));
        out
    }

    /// One RIFF chunk, padded to an even length as WebP requires.
    fn riff(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    /// A WebP with an extended header whose flags claim the chunks given.
    fn webp(flags: u8, chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        let mut header = vec![flags, 0, 0, 0];
        header.extend_from_slice(&[15, 0, 0, 15, 0, 0]); // 16x16
        body.extend_from_slice(&riff(b"VP8X", &header));
        for part in chunks {
            body.extend_from_slice(part);
        }
        body.extend_from_slice(&riff(b"VP8 ", &[0; 12]));

        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    // -----------------------------------------------------------------------
    // The promise: the picture is not touched
    // -----------------------------------------------------------------------

    /// The whole reason this is not an export. Everything from the scan marker
    /// onward must be the original's own bytes, because no encoder ran.
    #[test]
    fn a_scrubbed_jpeg_keeps_its_picture_byte_for_byte() {
        let original = jpeg(&[exif_segment(), xmp_segment(), profile_segment()]);
        let scrubbed = scrub(&original, Format::Jpeg, Keep::Profile).expect("a scrub");

        let scan_of = |bytes: &[u8]| {
            let at = bytes.windows(2).position(|pair| pair == [0xFF, 0xDA]).expect("a scan");
            bytes[at..].to_vec()
        };
        assert_eq!(scan_of(&scrubbed), scan_of(&original), "the picture's own bytes changed");
        assert!(scrubbed.len() < original.len(), "nothing was removed");
    }

    /// The entropy-coded data in the fixture contains `FF E1`, which is what
    /// APP1 looks like. A walk that carried on past the scan would find a
    /// segment there and mangle the picture — so this asserts on the shape of
    /// the file, not only its length.
    #[test]
    fn something_that_looks_like_a_marker_inside_the_picture_is_left_alone() {
        let original = jpeg(&[exif_segment()]);
        let scrubbed = scrub(&original, Format::Jpeg, Keep::Profile).expect("a scrub");
        // The scan's own bytes, the bare `FF E1` among them, arrive unchanged at
        // the end of the file.
        assert!(
            scrubbed.ends_with(&[0x12, 0x34, 0xFF, 0x00, 0x56, 0xFF, 0xE1, 0x78, 0xFF, 0xD9]),
            "the picture's tail was rewritten"
        );
        // Exactly one `FF E1` is left, and it is that one. Counted rather than
        // merely looked for: the pair is *supposed* to still be there, inside
        // the picture, and what must be gone is the real APP1 segment before the
        // scan. Asserting only its absence would fail on a correct scrub, and
        // asserting only its presence would pass on one that removed nothing.
        assert_eq!(
            scrubbed.windows(2).filter(|pair| *pair == [0xFF, 0xE1]).count(),
            1,
            "the APP1 segment before the scan survived, or the one inside the picture did not"
        );
    }

    // -----------------------------------------------------------------------
    // What goes and what stays
    // -----------------------------------------------------------------------

    #[test]
    fn a_jpeg_loses_its_exif_xmp_and_iptc_and_keeps_its_profile() {
        let original = jpeg(&[exif_segment(), xmp_segment(), iptc_segment(), profile_segment()]);

        let found = survey(&original, Format::Jpeg).expect("a survey");
        assert!(found.exif && found.xmp && found.iptc && found.profile, "{found:?}");
        assert!(found.anything(Keep::Profile));

        let scrubbed = scrub(&original, Format::Jpeg, Keep::Profile).expect("a scrub");
        let after = survey(&scrubbed, Format::Jpeg).expect("a survey");
        assert!(!after.exif && !after.xmp && !after.iptc, "{after:?} still describes the owner");
        assert!(after.profile, "the colour profile was taken away with the metadata");
        assert!(!after.anything(Keep::Profile), "a second scrub would still find something");
    }

    /// The profile is the one thing whose removal makes the picture *wrong*
    /// rather than anonymous, so it goes only when asked for by name.
    #[test]
    fn stripping_everything_takes_the_profile_too() {
        let original = jpeg(&[exif_segment(), profile_segment()]);
        let scrubbed = scrub(&original, Format::Jpeg, Keep::Nothing).expect("a scrub");
        let after = survey(&scrubbed, Format::Jpeg).expect("a survey");
        assert!(!after.profile, "the profile stayed when everything was asked to go");
        assert!(!after.exif);
    }

    /// A file that carries nothing must be reported as carrying nothing, so
    /// the interface can say so instead of rewriting it for no reason.
    #[test]
    fn a_file_with_nothing_to_strip_says_so() {
        let bare = jpeg(&[]);
        let found = survey(&bare, Format::Jpeg).expect("a survey");
        assert!(!found.anything(Keep::Profile));
        assert_eq!(found.describe(), "nothing to strip");

        // And one carrying only a profile has nothing to strip by default, but
        // something to strip when everything is asked for.
        let tagged = jpeg(&[profile_segment()]);
        let found = survey(&tagged, Format::Jpeg).expect("a survey");
        assert!(!found.anything(Keep::Profile), "a profile alone was counted as metadata");
        assert!(found.anything(Keep::Nothing));
    }

    /// An APP segment whose signature this module does not recognise is not
    /// metadata as far as it knows, and is kept. Dropping every unknown APP
    /// would be removing parts of files it cannot describe.
    #[test]
    fn an_unrecognised_app_segment_is_left_where_it_was() {
        let mine = segment(0xE1, b"Something Else\0and its payload");
        let original = jpeg(&[mine.clone(), exif_segment()]);
        let scrubbed = scrub(&original, Format::Jpeg, Keep::Profile).expect("a scrub");
        assert!(
            scrubbed.windows(mine.len()).any(|window| window == mine.as_slice()),
            "an APP1 that was not EXIF or XMP was dropped"
        );
    }

    /// A comment is text about the file, and goes.
    #[test]
    fn a_comment_goes_with_the_rest() {
        let original = jpeg(&[segment(0xFE, b"written by somebody")]);
        let scrubbed = scrub(&original, Format::Jpeg, Keep::Profile).expect("a scrub");
        assert!(
            !scrubbed.windows(9).any(|window| window == b"somebody\0"[..8].to_vec().as_slice()),
            "the comment survived"
        );
        assert!(scrubbed.len() < original.len());
    }

    // -----------------------------------------------------------------------
    // PNG
    // -----------------------------------------------------------------------

    #[test]
    fn a_png_loses_its_exif_text_and_time_and_keeps_its_profile() {
        let mut xmp = b"XML:com.adobe.xmp\0".to_vec();
        xmp.extend_from_slice(b"\0\0\0<x:xmpmeta>owner</x:xmpmeta>");

        let original = png(&[
            chunk(b"eXIf", b"II\x2a\x00\x08\x00\x00\x00"),
            chunk(b"iCCP", b"profile\0\0compressed"),
            chunk(b"iTXt", &xmp),
            chunk(b"tEXt", b"Software\0Some Editor"),
            chunk(b"tIME", &[0x07, 0xEA, 9, 19, 12, 0, 0]),
        ]);

        let found = survey(&original, Format::Png).expect("a survey");
        assert!(found.exif && found.xmp && found.profile, "{found:?}");

        let scrubbed = scrub(&original, Format::Png, Keep::Profile).expect("a scrub");
        let after = survey(&scrubbed, Format::Png).expect("a survey");
        assert!(!after.exif && !after.xmp, "{after:?}");
        assert!(after.profile, "the profile went with the metadata");

        // The text and the time are gone as bytes, not only as flags.
        assert!(!scrubbed.windows(4).any(|window| window == b"tEXt"), "a text chunk survived");
        assert!(!scrubbed.windows(4).any(|window| window == b"tIME"), "the timestamp survived");
        // And the picture is still there, with its header and its end marker.
        assert!(scrubbed.windows(4).any(|window| window == b"IDAT"));
        assert!(scrubbed.ends_with(&chunk(b"IEND", &[])));
    }

    /// The chunks that carry the picture must come across untouched, checksum
    /// included: a PNG whose `IDAT` was rewritten is a PNG that may not
    /// decode, and the checksum is what would catch it.
    #[test]
    fn a_scrubbed_png_keeps_its_picture_chunks_exactly() {
        let picture = chunk(b"IDAT", &[0x78, 0x01, 0x01, 0x00]);
        let original = png(&[chunk(b"tEXt", b"Comment\0hello")]);
        let scrubbed = scrub(&original, Format::Png, Keep::Profile).expect("a scrub");
        assert!(
            scrubbed.windows(picture.len()).any(|window| window == picture.as_slice()),
            "the picture chunk was not copied across as it was"
        );
    }

    // -----------------------------------------------------------------------
    // WebP
    // -----------------------------------------------------------------------

    #[test]
    fn a_webp_loses_its_exif_and_xmp_and_its_header_stops_claiming_them() {
        const ICC: u8 = 0b0010_0000;
        const EXIF: u8 = 0b0000_1000;
        const XMP: u8 = 0b0000_0100;

        let original = webp(
            ICC | EXIF | XMP,
            &[
                riff(b"ICCP", &[0; 16]),
                riff(b"EXIF", b"II\x2a\x00\x08\x00\x00\x00"),
                riff(b"XMP ", b"<x:xmpmeta>owner</x:xmpmeta>"),
            ],
        );

        let found = survey(&original, Format::WebP).expect("a survey");
        assert!(found.exif && found.xmp && found.profile, "{found:?}");

        let scrubbed = scrub(&original, Format::WebP, Keep::Profile).expect("a scrub");
        let after = survey(&scrubbed, Format::WebP).expect("a survey");
        assert!(!after.exif && !after.xmp, "{after:?}");
        assert!(after.profile);

        // The flags must describe the file that was written. A flag left set
        // for a chunk that is gone sends a decoder looking for nothing.
        let flags = scrubbed[scrubbed.windows(4).position(|window| window == b"VP8X").expect("the header") + 8];
        assert_eq!(flags & EXIF, 0, "the header still claims EXIF");
        assert_eq!(flags & XMP, 0, "the header still claims XMP");
        assert_eq!(flags & ICC, ICC, "the header stopped claiming a profile that is still there");
    }

    /// The size in a RIFF header is the one number this module computes rather
    /// than copies, so it is the one that can be wrong.
    #[test]
    fn a_scrubbed_webp_states_its_own_new_size() {
        let original = webp(0b0000_1000, &[riff(b"EXIF", &[0; 32])]);
        let scrubbed = scrub(&original, Format::WebP, Keep::Profile).expect("a scrub");

        let stated = u32::from_le_bytes([scrubbed[4], scrubbed[5], scrubbed[6], scrubbed[7]]) as usize;
        assert_eq!(stated, scrubbed.len() - 8, "the stated size does not match the file");
        assert!(scrubbed.len() < original.len());
    }

    // -----------------------------------------------------------------------
    // Refusals
    // -----------------------------------------------------------------------

    /// A format whose metadata cannot be dropped without rewriting offsets
    /// into the picture must refuse rather than appear to work.
    #[test]
    fn a_format_this_cannot_walk_refuses_by_name() {
        assert!(!supported(Format::Heic));
        assert!(!supported(Format::Avif));
        assert!(!supported(Format::JpegXl));
        assert!(supported(Format::Jpeg) && supported(Format::Png) && supported(Format::WebP));

        assert_eq!(survey(&[0; 32], Format::Heic), Err(Refusal::Unsupported));
        assert_eq!(scrub(&[0; 32], Format::Heic, Keep::Profile), Err(Refusal::Unsupported));
    }

    /// Bytes that are not the format they claim must not be rewritten into
    /// something that looks like a file.
    #[test]
    fn a_file_that_does_not_hold_together_refuses() {
        assert_eq!(survey(b"not a jpeg", Format::Jpeg), Err(Refusal::Malformed));
        assert_eq!(survey(b"not a png", Format::Png), Err(Refusal::Malformed));
        assert_eq!(survey(b"not a webp", Format::WebP), Err(Refusal::Malformed));

        // A JPEG whose segment length runs past the end of the file.
        let mut truncated = vec![0xFF, 0xD8, 0xFF, 0xE1];
        truncated.extend_from_slice(&9000u16.to_be_bytes());
        truncated.extend_from_slice(b"Exif\0\0");
        assert_eq!(survey(&truncated, Format::Jpeg), Err(Refusal::Malformed));

        // A PNG chunk whose length runs past the end.
        let mut png_bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png_bytes.extend_from_slice(&9000u32.to_be_bytes());
        png_bytes.extend_from_slice(b"tEXt");
        assert_eq!(survey(&png_bytes, Format::Png), Err(Refusal::Malformed));
    }

    // -----------------------------------------------------------------------
    // What the interface says
    // -----------------------------------------------------------------------

    #[test]
    fn what_is_carried_is_named_not_counted() {
        let found = Found {
            exif: true,
            xmp: true,
            iptc: false,
            profile: true,
            bytes: 4096,
        };
        assert_eq!(found.describe(), "EXIF, XMP (4.0 kB)");

        assert_eq!(readable(512), "512 B");
        assert_eq!(readable(1536), "1.5 kB");
        assert_eq!(readable(3 * 1024 * 1024), "3.0 MB");
    }

    /// Scrubbing twice must be the same as scrubbing once: the operation has
    /// to be idempotent, or a person who runs it on a folder twice gets two
    /// different files.
    #[test]
    fn scrubbing_twice_changes_nothing_the_second_time() {
        for (bytes, format) in [
            (jpeg(&[exif_segment(), xmp_segment(), profile_segment()]), Format::Jpeg),
            (png(&[chunk(b"eXIf", b"II\x2a\x00\x08\x00\x00\x00"), chunk(b"tEXt", b"K\0v")]), Format::Png),
            (
                webp(0b0010_1100, &[riff(b"ICCP", &[0; 8]), riff(b"EXIF", &[0; 8]), riff(b"XMP ", &[0; 8])]),
                Format::WebP,
            ),
        ] {
            let once = scrub(&bytes, format, Keep::Profile).expect("a scrub");
            let twice = scrub(&once, format, Keep::Profile).expect("a second scrub");
            assert_eq!(once, twice, "{format:?} was not left alone the second time");
        }
    }
}
