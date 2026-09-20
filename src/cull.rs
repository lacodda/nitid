//! Going through a shoot and saying which frames are worth keeping.
//!
//! The mark goes **into the file**, not into a small file beside it. That is
//! the whole point of the feature and it was settled by measurement rather
//! than by preference: a rating written into a photograph shows up as stars in
//! Explorer's own column, and one written to an XMP file next to it shows up
//! as "Unrated". A sidecar is a mark only the program that wrote it can see,
//! which is the opposite of what culling is for — the selection has to survive
//! being handed to something else. ADR 0029 records both measurements.
//!
//! **It takes two fields, because no single one can say all three things.**
//!
//! - *Keep* is the EXIF rating (`0x4746` in IFD0, with `0x4749` beside it),
//!   which is what Windows writes and reads, so a kept frame has a star in
//!   Explorer.
//! - *Reject* is `xmp:Rating="-1"` in an embedded XMP packet, which is
//!   Adobe's convention and what Lightroom and Bridge understand. EXIF has
//!   nowhere to put it: its rating field is `0..=5` and every value in it
//!   means some degree of wanting the picture.
//!
//! The obvious-looking alternative — a reject as a percentage too small for
//! any star — was tried and is wrong, and the way it is wrong is worth keeping
//! in mind: Windows derives the stars it shows *from the percentage* whenever
//! that field is set, so a reject written there appears in Explorer as a
//! one-star favourite. The failure is the inverse of the one the feature
//! exists to avoid, and no test that only read the file back with this
//! module's own reader could have seen it. `tests/live_cull.rs` asks the
//! Windows shell instead, which is the only reader whose answer settles it.
//!
//! Only the metadata is rewritten, never the pixels, so a marked JPEG is
//! identical from its start-of-scan marker onward — the same promise `rotate`
//! makes for a saved turn.
//!
//! Three marks on one scale rather than two independent flags. Keep and reject
//! could each have been a flag of its own, but then a file has two states of
//! judgement and "show me the marked ones" stops having one answer.

use std::path::Path;

use anyhow::{Context, Result};
use little_exif::exif_tag::ExifTag;
use little_exif::ifd::ExifTagGroup;
use little_exif::metadata::Metadata;

/// EXIF `Rating`, IFD0. What Explorer reads for its star column.
const RATING: u16 = 0x4746;

/// EXIF `RatingPercent`, IFD0. Windows writes both and so does this; a reader
/// that prefers the percentage gets one that agrees with the stars.
const RATING_PERCENT: u16 = 0x4749;

/// The `xmp:Rating` that means "rejected".
///
/// `-1` is Adobe's own convention, which Lightroom and Bridge write and read
/// for the reject flag, and it is outside the `0..=5` a rating can be — so a
/// reader that does not know the convention sees a value it cannot use rather
/// than a rating it will misreport. Explorer shows such a file as "Unrated",
/// measured, which is the honest answer: Windows has no reject to show.
const REJECTED_RATING: i32 = -1;

/// What has been said about a picture while going through a folder.
///
/// The names are the judgement, not the number: a person culling a shoot
/// thinks "keep this one", not "one star". The numbers are how the judgement
/// survives being written down.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Mark {
    /// Nothing said about it yet. Not a verdict — the frames nobody has
    /// reached are in this state, and so are the ones deliberately cleared.
    #[default]
    Unmarked,
    /// Worth keeping.
    Keep,
    /// Not worth keeping.
    ///
    /// A mark, emphatically not a delete. `Del` already sends a file to the
    /// recycle bin and always has; the point of a reject mark is to get
    /// through the folder *without* stopping to decide anything irreversible,
    /// and then look at what came out.
    Reject,
}

impl Mark {
    /// The EXIF rating this mark writes: one star for a keep, none otherwise.
    ///
    /// A reject writes no stars because EXIF has nowhere to say "rejected" —
    /// the field is `0..=5`, and every value in it is a degree of *wanting*
    /// the picture. The reject is carried in XMP instead; see
    /// [`REJECTED_RATING`].
    fn stars(self) -> u16 {
        match self {
            Self::Unmarked | Self::Reject => 0,
            Self::Keep => 1,
        }
    }

    /// The percentage written beside the stars, which has to agree with them.
    ///
    /// It is not a second, finer opinion: **Windows derives the stars it shows
    /// from this field whenever it is present and non-zero**, and ignores the
    /// star field when the two disagree. Measured over the pairs, with no
    /// stars set: percent 1, 2, 5 and 12 all show as "1 Star", 24 as "2
    /// Stars", 50 as "3 Stars", and only 0 or absent shows as "Unrated".
    ///
    /// That measurement is why the reject is not stored here. An earlier
    /// version wrote no stars and percent 1 for a reject, reasoning that a
    /// percentage too small for any star would be invisible to Explorer. It
    /// was visible, as one star — so every rejected frame would have appeared
    /// in Explorer as a favourite, which is worse than not marking it at all.
    fn percent(self) -> u16 {
        match self {
            Self::Unmarked | Self::Reject => 0,
            Self::Keep => 1,
        }
    }

    /// What the status line and a toast call it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unmarked => "unmarked",
            Self::Keep => "keep",
            Self::Reject => "reject",
        }
    }

    /// Whether the filter lets this one through.
    ///
    /// Reject is a mark, so a folder filtered to "marked" shows the rejects
    /// too: the point of the filter is to see what the pass produced, and
    /// hiding the rejects would hide half the answer.
    pub fn is_marked(self) -> bool {
        !matches!(self, Self::Unmarked)
    }
}

/// Read the mark a file carries.
///
/// Never an error: a file with no EXIF, a format that cannot carry any, and a
/// file that cannot be opened at all are the ordinary cases of "nothing said
/// about it", not failures to report. A viewer that showed an error banner
/// while stepping through a folder of PNGs would be unusable.
pub fn read(path: &Path) -> Mark {
    let Ok(bytes) = std::fs::read(path) else {
        return Mark::Unmarked;
    };
    from_bytes(&bytes)
}

/// The same, from bytes already in hand.
///
/// The viewer has the file in memory when it shows it, so the panel and the
/// status line read the mark without going back to the disk.
pub fn from_bytes(bytes: &[u8]) -> Mark {
    // XMP first, because it is the only place a reject can be stated and a
    // reject is the answer that has to win: a rejected file also carries no
    // stars, which the EXIF half alone would read as "nothing said".
    if xmp::rating_of(bytes) == Some(REJECTED_RATING) {
        return Mark::Reject;
    }

    let mut cursor = std::io::Cursor::new(bytes);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor) else {
        return Mark::Unmarked;
    };

    let field = |tag: u16| {
        exif.get_field(exif::Tag(exif::Context::Tiff, tag), exif::In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
    };

    from_exif(field(RATING), field(RATING_PERCENT))
}

/// Work out the mark from the two rating tags, either of which may be missing.
///
/// Split out from the reading so the rule itself can be tested over every
/// combination, including the ones no writer of ours produces: another
/// program's rating has to land somewhere sensible, and "somewhere sensible"
/// is a decision, not an accident.
///
/// Reject is not reachable here — it lives in XMP — so this answers only
/// "wanted" or "nothing said".
fn from_exif(stars: Option<u32>, percent: Option<u32>) -> Mark {
    // Any star at all is a keep. One star is what this viewer writes; two
    // through five come from somewhere else and are still a keep, because a
    // person who rated a photograph in another program has plainly said they
    // want it.
    //
    // The percentage counts for the same reason and on the same terms Windows
    // reads it: it is where Explorer's own stars come from, so a file rated
    // through the Properties dialog is a file someone rated.
    if matches!(stars, Some(1..)) || matches!(percent, Some(1..)) {
        Mark::Keep
    } else {
        Mark::Unmarked
    }
}

/// Write the mark into the file, leaving its pixels alone.
///
/// Fails rather than quietly doing nothing when the format cannot carry the
/// mark: a viewer that says it marked a file and did not is worse than one
/// that says the format has nowhere to put the mark. Same rule as the saved
/// turn.
///
/// Two halves, because one field cannot hold both answers. The stars go into
/// EXIF, where Windows reads them; the reject goes into XMP, where Lightroom
/// and Bridge read it and where Explorer correctly sees nothing.
pub fn write(path: &Path, mark: Mark) -> Result<()> {
    // A file with no EXIF at all is the ordinary case for anything a phone did
    // not take, and `new_from_path` fails outright on one — an empty block is
    // the right starting point, and the write below puts it into the file.
    let mut metadata = Metadata::new_from_path(path).unwrap_or_else(|_| Metadata::new());
    metadata.set_tag(ExifTag::UnknownINT16U(vec![mark.stars()], RATING, ExifTagGroup::GENERIC));
    metadata.set_tag(ExifTag::UnknownINT16U(vec![mark.percent()], RATING_PERCENT, ExifTagGroup::GENERIC));
    metadata.write_to_file(path).with_context(|| format!("{} cannot carry a mark", name_of(path)))?;

    // The XMP side is written second and over the result of the first: the
    // EXIF writer rebuilds the container, so a packet put in before it would
    // be the one thing that does not survive.
    let bytes = std::fs::read(path).with_context(|| format!("reading {} back", name_of(path)))?;
    let rating = (mark == Mark::Reject).then_some(REJECTED_RATING);
    if let Some(updated) = xmp::with_rating(&bytes, rating) {
        std::fs::write(path, updated).with_context(|| format!("writing {}", name_of(path)))?;
    }
    Ok(())
}

/// What to call a path in a message: its last component.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The one XMP property this viewer writes, and the container work to put it
/// in a file.
///
/// A whole XMP toolkit is not wanted here. The published ones wrap Adobe's C++
/// library, which this project refuses on principle — every decoder in it is
/// Rust precisely so that a malformed file costs a panic and never code
/// execution (ADR 0002, ADR 0007) — and the need is one attribute on one
/// element. So the packet is assembled as text and put into the container the
/// same way `scrub` takes segments out of it, one level down from the same
/// knowledge of what a JPEG is made of.
mod xmp {
    /// The APP1 signature that marks a JPEG segment as XMP rather than EXIF.
    const SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

    /// The packet, with the rating filled in.
    ///
    /// Written out in full rather than edited into whatever the file already
    /// carries. Editing would mean parsing arbitrary RDF to change one
    /// attribute, and getting that wrong means corrupting metadata this
    /// viewer did not write — a much worse outcome than replacing a packet
    /// whose only author is this viewer.
    ///
    /// The consequence is stated plainly: **a file whose XMP came from
    /// somewhere else loses it when a reject is written.** Acceptable because
    /// the alternative is an RDF editor, and because marking is a deliberate
    /// act on a photograph the person is looking at. Revisited if a real file
    /// ever arrives carrying XMP worth keeping.
    fn packet(rating: i32) -> Vec<u8> {
        let text = format!(
            concat!(
                r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>"#,
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/">"#,
                r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
                r#"<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="{}"/>"#,
                r#"</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#,
            ),
            rating,
        );
        let mut payload = SIGNATURE.to_vec();
        payload.extend_from_slice(text.as_bytes());
        payload
    }

    /// The `xmp:Rating` an XMP packet in these bytes states, if any.
    ///
    /// Read with a search rather than an XML parser, for the same reason the
    /// packet is written as text: one attribute, in a document whose shape is
    /// known. A packet this viewer did not write is read just as well, since
    /// the attribute is spelled the same way by everything that writes it.
    pub(super) fn rating_of(bytes: &[u8]) -> Option<i32> {
        let packet = find(bytes)?;
        let text = std::str::from_utf8(packet).ok()?;
        let at = text.find("xmp:Rating=")? + "xmp:Rating=".len();
        let rest = text.get(at..)?;
        let quote = rest.chars().next()?;
        let value = rest.get(1..)?.split(quote).next()?;
        value.trim().parse().ok()
    }

    /// The bytes of the XMP packet inside a JPEG, without its signature.
    ///
    /// `None` for anything that is not a JPEG carrying one. Other containers
    /// hold XMP in their own way; this viewer writes a reject only where it
    /// can, and says so rather than pretending otherwise.
    fn find(bytes: &[u8]) -> Option<&[u8]> {
        segment(bytes).map(|(start, end)| &bytes[start + 4 + SIGNATURE.len()..end])
    }

    /// Where the XMP segment sits in a JPEG: the range of the whole segment,
    /// marker and length included.
    fn segment(bytes: &[u8]) -> Option<(usize, usize)> {
        if !bytes.starts_with(&[0xFF, 0xD8]) {
            return None;
        }

        let mut at = 2usize;
        loop {
            // A run of 0xFF before a marker is legal padding.
            while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
                at += 1;
            }
            if bytes.get(at) != Some(&0xFF) {
                return None;
            }
            let &marker = bytes.get(at + 1)?;

            // Markers that stand alone, and the two that end the walk: the
            // scan, after which everything is picture, and end-of-image.
            if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                at += 2;
                continue;
            }
            if marker == 0xDA || marker == 0xD9 {
                return None;
            }

            let length = bytes.get(at + 2..at + 4).map(|pair| usize::from(u16::from_be_bytes([pair[0], pair[1]])))?;
            if length < 2 {
                return None;
            }
            let end = at + 2 + length;
            if end > bytes.len() {
                return None;
            }

            if marker == 0xE1 && bytes.get(at + 4..end).is_some_and(|payload| payload.starts_with(SIGNATURE)) {
                return Some((at, end));
            }
            at = end;
        }
    }

    /// The file with its XMP rating set to `rating`, or with its XMP packet
    /// taken out when there is none to state.
    ///
    /// `None` when nothing needs to change, and for a container this does not
    /// know how to edit — the caller leaves the file alone in both cases,
    /// which is why they are one answer.
    pub(super) fn with_rating(bytes: &[u8], rating: Option<i32>) -> Option<Vec<u8>> {
        let existing = segment(bytes);

        match (rating, existing) {
            // Nothing to say and nothing said: leave the file alone rather
            // than rewriting it to the same thing.
            (None, None) => None,
            // Nothing to say any more. The packet goes, rather than staying
            // with a rating of zero: a cleared mark should leave a file the
            // way it found it as nearly as it can.
            (None, Some((start, end))) => {
                let mut out = Vec::with_capacity(bytes.len() - (end - start));
                out.extend_from_slice(&bytes[..start]);
                out.extend_from_slice(&bytes[end..]);
                Some(out)
            }
            (Some(rating), existing) => {
                // Only a JPEG can be given a packet it does not have: putting
                // one into another container means knowing that container's
                // rules, which this does not claim to.
                if existing.is_none() && !bytes.starts_with(&[0xFF, 0xD8]) {
                    return None;
                }

                let payload = packet(rating);
                // The segment's length field counts itself, so two more than
                // the payload. A packet large enough to overflow it is not one
                // this module can produce — the text is fixed but for a small
                // number — and refusing beats writing a length that lies.
                let length = u16::try_from(payload.len() + 2).ok()?;

                let mut replacement = Vec::with_capacity(payload.len() + 4);
                replacement.extend_from_slice(&[0xFF, 0xE1]);
                replacement.extend_from_slice(&length.to_be_bytes());
                replacement.extend_from_slice(&payload);

                let mut out = Vec::with_capacity(bytes.len() + replacement.len());
                match existing {
                    Some((start, end)) => {
                        out.extend_from_slice(&bytes[..start]);
                        out.extend_from_slice(&replacement);
                        out.extend_from_slice(&bytes[end..]);
                    }
                    // Straight after the start-of-image marker, which is where
                    // every APP segment is allowed to be and where a reader
                    // looking for metadata looks first.
                    None => {
                        out.extend_from_slice(&bytes[..2]);
                        out.extend_from_slice(&replacement);
                        out.extend_from_slice(&bytes[2..]);
                    }
                }
                Some(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Somewhere to write files that is not the owner's disk proper.
    fn sandbox(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join("nitid-cull-tests").join(name);
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a sandbox to write into");
        directory
    }

    /// A JPEG built here rather than committed: a fixture from the owner's
    /// archive would put a real photograph in a public repository.
    ///
    /// Noise at a quality the default encoder does not use, for the reason
    /// `rotate`'s fixture is: a smooth gradient at the default quality
    /// re-encodes to the same bytes, and a test comparing scan data could
    /// then not tell a re-encoding implementation from this one.
    fn a_jpeg(path: &Path) {
        let mut pixels = image::RgbImage::new(96, 64);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for pixel in pixels.pixels_mut() {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed >> 24) as u8
            };
            *pixel = image::Rgb([next(), next(), next()]);
        }
        let file = std::fs::File::create(path).expect("a file to write");
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 93);
        encoder.encode_image(&pixels).expect("an encoded jpeg");
    }

    /// The compressed image data: everything from the start-of-scan marker on.
    fn scan_data(path: &Path) -> Vec<u8> {
        let bytes = std::fs::read(path).expect("the file back");
        bytes
            .windows(2)
            .position(|pair| pair == [0xFF, 0xDA])
            .map(|start| bytes[start..].to_vec())
            .expect("a jpeg has a start-of-scan marker")
    }

    #[test]
    fn a_mark_survives_being_written_and_read_back() {
        let directory = sandbox("round-trip");
        for mark in [Mark::Keep, Mark::Reject, Mark::Unmarked] {
            let path = directory.join(format!("{}.jpg", mark.name()));
            a_jpeg(&path);
            assert_eq!(read(&path), Mark::Unmarked, "a fresh file starts unmarked");

            write(&path, mark).expect("the mark goes in");
            assert_eq!(read(&path), mark, "{} did not come back", mark.name());
        }
    }

    /// The promise of the feature: the mark is written and the pixels are not
    /// touched.
    ///
    /// Mutating `write` into something that decodes and re-encodes would keep
    /// the round-trip test green and fail here.
    #[test]
    fn marking_a_file_leaves_its_pixels_exactly_as_they_were() {
        let directory = sandbox("lossless");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);
        let before = scan_data(&path);

        write(&path, Mark::Keep).expect("the mark goes in");

        assert_eq!(scan_data(&path), before, "the compressed image data changed — something re-encoded the picture");
    }

    /// Changing one's mind has to be as cheap as the first press, including
    /// the way back to nothing.
    #[test]
    fn a_mark_can_be_changed_and_cleared() {
        let directory = sandbox("change");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);

        write(&path, Mark::Keep).expect("keep");
        assert_eq!(read(&path), Mark::Keep);

        write(&path, Mark::Reject).expect("reject");
        assert_eq!(read(&path), Mark::Reject, "a reject did not replace the keep");

        write(&path, Mark::Unmarked).expect("clear");
        assert_eq!(read(&path), Mark::Unmarked, "clearing left the file marked");
    }

    /// The rule that reads a rating, over every pair either tag can present —
    /// including the ones this viewer never writes.
    #[test]
    fn the_rating_of_another_program_lands_somewhere_sensible() {
        // What we write for a keep, and for nothing.
        assert_eq!(from_exif(Some(1), Some(1)), Mark::Keep);
        assert_eq!(from_exif(Some(0), Some(0)), Mark::Unmarked);

        // What Windows writes: stars with a matching percentage. Every one of
        // them is a person saying they want the picture.
        for (stars, percent) in [(1, 1), (2, 25), (3, 50), (4, 75), (5, 99)] {
            assert_eq!(from_exif(Some(stars), Some(percent)), Mark::Keep, "{stars} stars should read as a keep");
        }

        // A file with neither tag, or with only one of them.
        assert_eq!(from_exif(None, None), Mark::Unmarked);
        assert_eq!(from_exif(None, Some(0)), Mark::Unmarked);
        assert_eq!(from_exif(Some(0), None), Mark::Unmarked);
        assert_eq!(from_exif(Some(3), None), Mark::Keep, "stars alone are still a rating");
        // A percentage with no stars is a rating too, and this is not a
        // detail: it is where Explorer's own stars come from when a file is
        // rated through the Properties dialog. Reading it as "nothing said"
        // would mean disagreeing with what Windows is visibly showing.
        assert_eq!(from_exif(None, Some(75)), Mark::Keep);
        assert_eq!(from_exif(Some(0), Some(50)), Mark::Keep);
    }

    /// A reject has to win over the stars beside it.
    ///
    /// A rejected file carries no stars, so a reader that consulted EXIF
    /// first would call it unmarked and the reject would vanish — which is
    /// the one mistake that cannot be seen by looking at the file this viewer
    /// wrote, because both fields are its own.
    #[test]
    fn a_reject_is_read_even_though_it_carries_no_stars() {
        let directory = sandbox("reject-wins");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);

        write(&path, Mark::Reject).expect("the mark goes in");

        let bytes = std::fs::read(&path).expect("the file back");
        assert_eq!(xmp::rating_of(&bytes), Some(-1), "the reject was not written as an XMP rating");
        assert_eq!(from_bytes(&bytes), Mark::Reject);

        // And the EXIF half says nothing that Windows would turn into a star.
        let mut cursor = std::io::Cursor::new(&bytes);
        let exif = exif::Reader::new().read_from_container(&mut cursor).expect("exif");
        for tag in [RATING, RATING_PERCENT] {
            let value = exif
                .get_field(exif::Tag(exif::Context::Tiff, tag), exif::In::PRIMARY)
                .and_then(|field| field.value.get_uint(0));
            assert!(
                matches!(value, None | Some(0)),
                "a rejected frame carries {tag:#x} = {value:?}, which Windows would show as a star",
            );
        }
    }

    /// Taking a mark off takes the XMP packet out again, rather than leaving
    /// one behind that says nothing.
    #[test]
    fn clearing_a_reject_takes_the_packet_out() {
        let directory = sandbox("packet-removed");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);

        write(&path, Mark::Reject).expect("reject");
        assert!(xmp::rating_of(&std::fs::read(&path).unwrap()).is_some());

        write(&path, Mark::Unmarked).expect("clear");
        assert_eq!(xmp::rating_of(&std::fs::read(&path).unwrap()), None, "an empty packet was left in the file");
        assert_eq!(read(&path), Mark::Unmarked);
    }

    /// Marking the same file twice must not stack packets up inside it.
    ///
    /// The segment is replaced where it stands rather than added again; a
    /// writer that appended would grow the file on every press of `X`, which
    /// a person culling a folder would notice only much later.
    #[test]
    fn marking_repeatedly_leaves_one_packet() {
        let directory = sandbox("one-packet");
        let path = directory.join("photo.jpg");
        a_jpeg(&path);

        write(&path, Mark::Reject).expect("first");
        let after_one = std::fs::read(&path).expect("the file back");

        for _ in 0..4 {
            write(&path, Mark::Reject).expect("again");
        }
        let after_five = std::fs::read(&path).expect("the file back");

        assert_eq!(after_five.len(), after_one.len(), "the file grew: a packet was added rather than replaced");
        assert_eq!(from_bytes(&after_five), Mark::Reject);
    }

    /// A rating another program wrote as XMP is read, not just our own.
    #[test]
    fn an_xmp_rating_from_elsewhere_is_understood() {
        // Single quotes and spacing a different writer might use, which the
        // reader has to cope with because it is reading a convention rather
        // than its own output.
        let packet = br#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description xmp:Rating='-1'/></rdf:RDF></x:xmpmeta>"#;
        let bytes = jpeg_carrying_xmp(packet);

        assert_eq!(xmp::rating_of(&bytes), Some(-1));
        assert_eq!(from_bytes(&bytes), Mark::Reject);
    }

    /// A JPEG with the given XMP payload in an APP1 segment.
    fn jpeg_carrying_xmp(packet: &[u8]) -> Vec<u8> {
        let directory = sandbox("carrier");
        let path = directory.join("carrier.jpg");
        a_jpeg(&path);
        let original = std::fs::read(&path).expect("the file back");

        let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        payload.extend_from_slice(packet);
        let length = u16::try_from(payload.len() + 2).expect("a segment that fits");

        let mut out = original[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&payload);
        out.extend_from_slice(&original[2..]);
        out
    }

    /// The filter's question, answered for each of the three.
    #[test]
    fn the_filter_lets_through_everything_that_was_judged() {
        assert!(Mark::Keep.is_marked());
        assert!(
            Mark::Reject.is_marked(),
            "a rejected frame was judged — the filter has to show what the pass produced"
        );
        assert!(!Mark::Unmarked.is_marked());
    }

    /// Reading a file that is not there, and one that carries no EXIF, are
    /// both "nothing said about it" rather than errors.
    #[test]
    fn a_file_with_nothing_to_say_reads_as_unmarked() {
        let directory = sandbox("silent");
        assert_eq!(read(&directory.join("does-not-exist.jpg")), Mark::Unmarked);

        let path = directory.join("plain.png");
        image::RgbImage::new(4, 4).save(&path).expect("a png");
        assert_eq!(read(&path), Mark::Unmarked);
    }
}
