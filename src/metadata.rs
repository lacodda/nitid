//! What a file says about itself: the EXIF a camera wrote, and the facts the
//! viewer knows anyway.
//!
//! Read once when a picture is opened rather than when the panel is, which
//! costs 0.03 to 1.7 ms measured across the formats that carry EXIF at all —
//! cheap enough that deferring it would buy a wait on the first `I` in
//! exchange for nothing.
//!
//! Everything here is presentation: the values are formatted the way a
//! photographer reads them (`1/250 s`, `f/2.8`, `35 mm`), not the way the
//! standard stores them. A field that cannot be read is simply absent — a
//! camera writing something unexpected is not the viewer's problem to report.

use std::io::Cursor;

/// One line of the Info panel: what it is, and what it says.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub label: &'static str,
    pub value: String,
}

/// Everything the panel shows about one file.
#[derive(Clone, Debug, Default)]
pub struct Metadata {
    /// Camera, exposure, lens — the fields grouped as a photographer reads
    /// them. Empty when the file carries no EXIF, which is the common case:
    /// every screenshot and most PNGs have none.
    pub camera: Vec<Entry>,
    /// Where the photograph was taken, as decimal degrees.
    ///
    /// Kept apart from the rest because it is the one field that is about the
    /// person rather than the picture, and because it is what a click copies.
    pub location: Option<Location>,
}

/// A place, as EXIF states it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

impl Location {
    /// The pair as a person would paste it into a map: decimal degrees, north
    /// and east positive, six places — about a tenth of a metre, which is
    /// finer than any camera's fix.
    pub fn as_text(self) -> String {
        format!("{:.6}, {:.6}", self.latitude, self.longitude)
    }
}

/// Read what the file says about itself.
///
/// Returns an empty set rather than an error when there is no EXIF: a file
/// without metadata is the ordinary case, not a failure.
pub fn read(bytes: &[u8]) -> Metadata {
    let Some(exif) = read_exif(bytes) else {
        return Metadata::default();
    };

    Metadata {
        camera: camera_entries(&exif),
        location: location(&exif),
    }
}

fn read_exif(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = Cursor::new(bytes);
    exif::Reader::new().read_from_container(&mut cursor).ok()
}

/// The fields worth a line, in the order a photographer reads them: what took
/// the picture, how it was exposed, and when.
fn camera_entries(exif: &exif::Exif) -> Vec<Entry> {
    let mut entries = Vec::new();

    // The camera itself. Make and Model are one line, because "SONY" and
    // "ILCE-7M4" on separate rows say less than they do together.
    let make = text(exif, exif::Tag::Make);
    let model = text(exif, exif::Tag::Model);
    match (make, model) {
        (Some(make), Some(model)) => {
            // Most makers repeat themselves — "NIKON CORPORATION" then
            // "NIKON D850" — and printing both reads as a stutter.
            let value = if model.starts_with(&make) { model } else { format!("{make} {model}") };
            entries.push(Entry { label: "Camera", value });
        }
        (Some(value), None) | (None, Some(value)) => entries.push(Entry { label: "Camera", value }),
        (None, None) => {}
    }

    if let Some(lens) = text(exif, exif::Tag::LensModel) {
        entries.push(Entry { label: "Lens", value: lens });
    }

    // The exposure, as it is written on a photograph's back.
    if let Some(exposure) = exposure_time(exif) {
        entries.push(Entry {
            label: "Exposure",
            value: exposure,
        });
    }
    if let Some(aperture) = rational(exif, exif::Tag::FNumber) {
        entries.push(Entry {
            label: "Aperture",
            value: format!("f/{}", trim(aperture)),
        });
    }
    if let Some(iso) = uint(exif, exif::Tag::PhotographicSensitivity) {
        entries.push(Entry {
            label: "ISO",
            value: iso.to_string(),
        });
    }
    if let Some(focal) = rational(exif, exif::Tag::FocalLength) {
        // The 35 mm equivalent in brackets, because a focal length means
        // nothing without the sensor it was measured on.
        let equivalent = uint(exif, exif::Tag::FocalLengthIn35mmFilm);
        let value = match equivalent {
            Some(equivalent) => format!("{} mm ({equivalent} mm eq.)", trim(focal)),
            None => format!("{} mm", trim(focal)),
        };
        entries.push(Entry { label: "Focal length", value });
    }

    if let Some(taken) = text(exif, exif::Tag::DateTimeOriginal).or_else(|| text(exif, exif::Tag::DateTime)) {
        entries.push(Entry {
            label: "Taken",
            value: readable_date(&taken),
        });
    }

    entries
}

/// EXIF writes the date as `2026:08:28 14:03:11`, with colons where a person
/// expects dashes. Everything else is left alone, including a date this build
/// does not recognise: showing it as written beats hiding it.
fn readable_date(raw: &str) -> String {
    match raw.split_once(' ') {
        Some((date, time)) if date.matches(':').count() == 2 => format!("{} {time}", date.replace(':', "-")),
        _ => raw.to_string(),
    }
}

/// Shutter speed, as a photographer says it: `1/250 s` below a second and
/// `2 s` above, rather than the raw rational either way.
fn exposure_time(exif: &exif::Exif) -> Option<String> {
    let value = rational(exif, exif::Tag::ExposureTime)?;
    if value <= 0.0 {
        return None;
    }
    if value >= 1.0 {
        return Some(format!("{} s", trim(value)));
    }
    // A camera stores 1/250 as 0.004, and the reciprocal is what is written on
    // the dial. Rounded, because 1/249.99 is the same picture.
    Some(format!("1/{} s", (1.0 / value).round() as u64))
}

fn text(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let exif::Value::Ascii(ref lines) = field.value else {
        return None;
    };
    let text = lines.iter().map(|line| String::from_utf8_lossy(line).to_string()).collect::<Vec<_>>().join(" ");
    let trimmed = text.trim().to_string();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn uint(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    exif.get_field(tag, exif::In::PRIMARY)?.value.get_uint(0)
}

fn rational(exif: &exif::Exif, tag: exif::Tag) -> Option<f64> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    match field.value {
        exif::Value::Rational(ref values) => values.first().map(|value| value.to_f64()),
        exif::Value::SRational(ref values) => values.first().map(|value| value.to_f64()),
        _ => None,
    }
}

/// Drop the decimals a number does not need: `2.8` stays, `35.0` becomes `35`.
fn trim(value: f64) -> String {
    let text = format!("{value:.1}");
    text.strip_suffix(".0").map(str::to_string).unwrap_or(text)
}

/// Where the photograph was taken.
///
/// Both halves must be present and well formed; a file with a latitude and no
/// longitude says nothing about a place, and is treated as saying nothing.
fn location(exif: &exif::Exif) -> Option<Location> {
    let latitude = degrees(exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, b'S')?;
    let longitude = degrees(exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, b'W')?;
    Some(Location { latitude, longitude })
}

/// One coordinate: three rationals for degrees, minutes and seconds, plus the
/// hemisphere letter that decides the sign.
fn degrees(exif: &exif::Exif, tag: exif::Tag, reference: exif::Tag, negative: u8) -> Option<f64> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let exif::Value::Rational(ref parts) = field.value else {
        return None;
    };
    if parts.len() < 3 {
        return None;
    }

    let value = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    if !value.is_finite() {
        return None;
    }

    // The reference letter is what makes a coordinate a place rather than a
    // magnitude: without it, south and north are the same number.
    let south_or_west = exif
        .get_field(reference, exif::In::PRIMARY)
        .and_then(|field| match field.value {
            exif::Value::Ascii(ref lines) => lines.first().and_then(|line| line.first()).copied(),
            _ => None,
        })
        .is_some_and(|letter| letter.eq_ignore_ascii_case(&negative));

    Some(if south_or_west { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an EXIF block by hand.
    ///
    /// Written here rather than taken from a library because the obvious
    /// library lies: `piexif` emits sub-IFDs that `kamadak-exif` rejects
    /// outright ("Truncated next IFD offset"), so a fixture built with it
    /// would test the reader against a broken file and pass for the wrong
    /// reason. Measured, not assumed — a hand-built sub-IFD reads fine.
    #[derive(Default)]
    struct Exif {
        zeroth: Vec<(u16, Value)>,
        sub: Vec<(u16, Value)>,
        gps: Vec<(u16, Value)>,
    }

    enum Value {
        Short(u16),
        Long(u32),
        Ascii(&'static str),
        Rational(Vec<(u32, u32)>),
    }

    impl Exif {
        fn zeroth(mut self, tag: u16, value: Value) -> Self {
            self.zeroth.push((tag, value));
            self
        }

        fn sub(mut self, tag: u16, value: Value) -> Self {
            self.sub.push((tag, value));
            self
        }

        fn gps(mut self, tag: u16, value: Value) -> Self {
            self.gps.push((tag, value));
            self
        }

        /// Lay the whole thing out: IFD0, then the sub-IFDs it points at, then
        /// the values too long to sit inside an entry.
        fn build(self) -> Vec<u8> {
            let mut tiff: Vec<u8> = b"II\x2a\x00".to_vec();
            tiff.extend_from_slice(&8u32.to_le_bytes());

            // IFD0 is laid out once with placeholder pointers to learn how
            // long it is — including its own out-of-line values — and once
            // more with the real ones. Two passes rather than arithmetic,
            // because the arithmetic is exactly what a fixture gets wrong.
            let mut entries: Vec<(u16, Value)> = self.zeroth;
            if !self.sub.is_empty() {
                entries.push((0x8769, Value::Long(0)));
            }
            if !self.gps.is_empty() {
                entries.push((0x8825, Value::Long(0)));
            }
            // Entries must be in ascending tag order.
            entries.sort_by_key(|(tag, _)| *tag);

            let zeroth_len = ifd(&entries, 8).len() as u32;
            let mut tail = Vec::new();
            let sub_at = (!self.sub.is_empty()).then(|| {
                let at = 8 + zeroth_len + tail.len() as u32;
                tail.extend_from_slice(&ifd(&self.sub, at));
                at
            });
            let gps_at = (!self.gps.is_empty()).then(|| {
                let at = 8 + zeroth_len + tail.len() as u32;
                tail.extend_from_slice(&ifd(&self.gps, at));
                at
            });

            for (tag, value) in &mut entries {
                let at = match *tag {
                    0x8769 => sub_at,
                    0x8825 => gps_at,
                    _ => None,
                };
                if let Some(at) = at {
                    *value = Value::Long(at);
                }
            }

            tiff.extend_from_slice(&ifd(&entries, 8));
            tiff.extend_from_slice(&tail);
            tiff
        }
    }

    /// One IFD at `at`, with its out-of-line values laid out after it.
    fn ifd(entries: &[(u16, Value)], at: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());

        let mut overflow = Vec::new();
        let overflow_at = at + 2 + entries.len() as u32 * 12 + 4;

        for (tag, value) in entries {
            out.extend_from_slice(&tag.to_le_bytes());
            let (kind, count, bytes) = value.encode();
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            if bytes.len() <= 4 {
                let mut padded = bytes.clone();
                padded.resize(4, 0);
                out.extend_from_slice(&padded);
            } else {
                out.extend_from_slice(&(overflow_at + overflow.len() as u32).to_le_bytes());
                overflow.extend_from_slice(&bytes);
            }
        }

        out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
        out.extend_from_slice(&overflow);
        out
    }

    impl Value {
        /// The type number, the count, and the bytes the standard stores.
        fn encode(&self) -> (u16, u32, Vec<u8>) {
            match self {
                Value::Short(value) => (3, 1, value.to_le_bytes().to_vec()),
                Value::Long(value) => (4, 1, value.to_le_bytes().to_vec()),
                Value::Ascii(text) => {
                    let mut bytes = text.as_bytes().to_vec();
                    bytes.push(0);
                    (2, bytes.len() as u32, bytes)
                }
                Value::Rational(parts) => {
                    let mut bytes = Vec::new();
                    for (numerator, denominator) in parts {
                        bytes.extend_from_slice(&numerator.to_le_bytes());
                        bytes.extend_from_slice(&denominator.to_le_bytes());
                    }
                    (5, parts.len() as u32, bytes)
                }
            }
        }
    }

    /// A JPEG carrying `tiff` in its APP1 segment, which is where a camera
    /// puts it and where the reader looks.
    fn jpeg_with(tiff: &[u8]) -> Vec<u8> {
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(tiff);

        let mut out = vec![0xFF, 0xD8]; // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        // A minimal rest-of-file: the reader stops at the segment it wants.
        out.extend_from_slice(&[0xFF, 0xD9]); // EOI
        out
    }

    fn photograph() -> Vec<u8> {
        jpeg_with(
            &Exif::default()
                .zeroth(0x010F, Value::Ascii("NITID"))
                .zeroth(0x0110, Value::Ascii("Probe One"))
                .sub(0x829A, Value::Rational(vec![(1, 250)]))
                .sub(0x829D, Value::Rational(vec![(28, 10)]))
                .sub(0x8827, Value::Short(400))
                .sub(0x920A, Value::Rational(vec![(35, 1)]))
                .sub(0xA405, Value::Short(52))
                .sub(0xA434, Value::Ascii("NITID 35mm f/1.8"))
                .sub(0x9003, Value::Ascii("2026:08:28 14:03:11"))
                .build(),
        )
    }

    fn value_of(metadata: &Metadata, label: &str) -> Option<String> {
        metadata.camera.iter().find(|entry| entry.label == label).map(|entry| entry.value.clone())
    }

    #[test]
    fn a_photograph_reads_as_a_photographer_would_write_it() {
        let metadata = read(&photograph());

        assert_eq!(value_of(&metadata, "Camera").as_deref(), Some("NITID Probe One"));
        assert_eq!(value_of(&metadata, "Lens").as_deref(), Some("NITID 35mm f/1.8"));
        // A shutter speed is read off the dial, not off the rational.
        assert_eq!(value_of(&metadata, "Exposure").as_deref(), Some("1/250 s"));
        assert_eq!(value_of(&metadata, "Aperture").as_deref(), Some("f/2.8"));
        assert_eq!(value_of(&metadata, "ISO").as_deref(), Some("400"));
        assert_eq!(value_of(&metadata, "Focal length").as_deref(), Some("35 mm (52 mm eq.)"));
        assert_eq!(value_of(&metadata, "Taken").as_deref(), Some("2026-08-28 14:03:11"));
    }

    /// A file with no EXIF is the ordinary case — every screenshot, most PNGs
    /// — and must read as "nothing to say" rather than as a failure.
    #[test]
    fn a_file_without_exif_says_nothing_and_does_not_fail() {
        let metadata = read(&[0xFF, 0xD8, 0xFF, 0xD9]);
        assert!(metadata.camera.is_empty());
        assert!(metadata.location.is_none());

        // And so does something that is not an image at all.
        let metadata = read(b"not an image");
        assert!(metadata.camera.is_empty());
    }

    /// A maker that repeats itself in the model must not stutter.
    #[test]
    fn a_camera_that_names_itself_twice_is_named_once() {
        let stuttering = jpeg_with(
            &Exif::default()
                .zeroth(0x010F, Value::Ascii("NIKON CORPORATION"))
                .zeroth(0x0110, Value::Ascii("NIKON CORPORATION D850"))
                .build(),
        );
        assert_eq!(value_of(&read(&stuttering), "Camera").as_deref(), Some("NIKON CORPORATION D850"));

        // And one that does not repeat itself is shown in full.
        let plain = jpeg_with(
            &Exif::default()
                .zeroth(0x010F, Value::Ascii("SONY"))
                .zeroth(0x0110, Value::Ascii("ILCE-7M4"))
                .build(),
        );
        assert_eq!(value_of(&read(&plain), "Camera").as_deref(), Some("SONY ILCE-7M4"));
    }

    /// A second or longer is written as seconds, not as a reciprocal: `1/0` is
    /// not a shutter speed anybody recognises.
    #[test]
    fn a_long_exposure_is_written_in_seconds() {
        let long = jpeg_with(&Exif::default().sub(0x829A, Value::Rational(vec![(2, 1)])).build());
        assert_eq!(value_of(&read(&long), "Exposure").as_deref(), Some("2 s"));

        let half = jpeg_with(&Exif::default().sub(0x829A, Value::Rational(vec![(1, 2)])).build());
        assert_eq!(value_of(&read(&half), "Exposure").as_deref(), Some("1/2 s"));

        // A zero exposure is not a picture; it is a file saying nothing.
        let zero = jpeg_with(&Exif::default().sub(0x829A, Value::Rational(vec![(0, 1)])).build());
        assert_eq!(value_of(&read(&zero), "Exposure"), None);
    }

    /// The hemisphere letters are what make a coordinate a place. Without
    /// them south and north are the same number, and a photograph taken in
    /// Asuncion would be placed in Siberia.
    #[test]
    fn the_hemisphere_decides_the_sign() {
        let southwest = jpeg_with(
            &Exif::default()
                .gps(1, Value::Ascii("S"))
                .gps(2, Value::Rational(vec![(25, 1), (15, 1), (4932, 100)]))
                .gps(3, Value::Ascii("W"))
                .gps(4, Value::Rational(vec![(57, 1), (34, 1), (3324, 100)]))
                .build(),
        );
        let place = read(&southwest).location.expect("a location");
        assert!((place.latitude - -25.2637).abs() < 0.0001, "latitude was {}", place.latitude);
        assert!((place.longitude - -57.5759).abs() < 0.0001, "longitude was {}", place.longitude);

        let northeast = jpeg_with(
            &Exif::default()
                .gps(1, Value::Ascii("N"))
                .gps(2, Value::Rational(vec![(25, 1), (15, 1), (4932, 100)]))
                .gps(3, Value::Ascii("E"))
                .gps(4, Value::Rational(vec![(57, 1), (34, 1), (3324, 100)]))
                .build(),
        );
        let place = read(&northeast).location.expect("a location");
        assert!(place.latitude > 25.0 && place.longitude > 57.0, "{place:?} did not stay positive");
    }

    /// Half a coordinate is not a place.
    #[test]
    fn a_latitude_without_a_longitude_is_not_a_location() {
        let half = jpeg_with(
            &Exif::default()
                .gps(1, Value::Ascii("S"))
                .gps(2, Value::Rational(vec![(25, 1), (15, 1), (4932, 100)]))
                .build(),
        );
        assert!(read(&half).location.is_none(), "half a coordinate was read as a place");
    }

    #[test]
    fn a_location_is_written_the_way_a_map_expects_it() {
        let place = Location {
            latitude: -25.2637,
            longitude: -57.5759,
        };
        assert_eq!(place.as_text(), "-25.263700, -57.575900");
    }

    /// A date the standard's way round, and one that is not.
    #[test]
    fn a_date_is_shown_with_dashes_but_an_unexpected_one_is_left_alone() {
        assert_eq!(readable_date("2026:08:28 14:03:11"), "2026-08-28 14:03:11");
        assert_eq!(readable_date("yesterday"), "yesterday");
        // A time that is itself colon-separated must not be rewritten.
        assert_eq!(readable_date("2026:08:28 14:03:11").split(' ').nth(1), Some("14:03:11"));
    }
}
