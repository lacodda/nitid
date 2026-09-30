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
    /// How the picture was taken: the camera, the exposure, the moment.
    ///
    /// Held as values rather than as the lines the panel shows, because two
    /// readers need them: the Info panel writes them the way a photographer
    /// reads them, and a comparison measures the distance between two of them
    /// — a stop of exposure, four tenths of a second — which a string that
    /// already says `1/250 s` cannot be asked for.
    pub shot: Shot,
    /// Where the photograph was taken, as decimal degrees.
    ///
    /// Kept apart from the rest because it is the one field that is about the
    /// person rather than the picture, and because it is what a click copies.
    pub location: Option<Location>,
}

impl Metadata {
    /// The lines of the panel's Camera section, in the order a photographer
    /// reads them: what took the picture, how it was exposed, and when.
    ///
    /// Empty when the file carries no EXIF, which is the common case: every
    /// screenshot and most PNGs have none.
    pub fn camera(&self) -> Vec<Entry> {
        let shot = &self.shot;
        let mut entries = Vec::new();
        let mut push = |label: &'static str, value: Option<String>| {
            if let Some(value) = value {
                entries.push(Entry { label, value });
            }
        };

        push("Camera", shot.camera.clone());
        push("Lens", shot.lens.clone());
        push("Exposure", shot.exposure.map(exposure_text));
        push("Aperture", shot.aperture.map(aperture_text));
        push("ISO", shot.iso.map(|iso| iso.to_string()));
        // Only when it was dialled in. Every ordinary frame carries a zero
        // here, and "0 EV" on every photograph is a line saying nothing.
        push("Compensation", shot.compensation.filter(|value| value.abs() >= 0.05).map(compensation_text));
        push("Focal length", shot.focal_length.map(|focal| focal_text(focal, shot.equivalent)));
        push("Taken", shot.taken.as_ref().map(Taken::text));
        entries
    }
}

/// How a picture was taken, as the camera wrote it down.
///
/// Every field is optional on its own: a phone writes no lens, a scan writes
/// no exposure, and a field that is missing is simply absent rather than a
/// reason to distrust the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shot {
    /// Make and model as one name, because "SONY" and "ILCE-7M4" on separate
    /// rows say less than they do together.
    pub camera: Option<String>,
    pub lens: Option<String>,
    /// Shutter speed, in seconds.
    pub exposure: Option<f64>,
    /// The f-number.
    pub aperture: Option<f64>,
    pub iso: Option<u32>,
    /// Exposure compensation, in stops. Kept at zero rather than dropped: a
    /// bracketed series is told apart by exactly this, and the frame at zero
    /// is one of the three.
    pub compensation: Option<f64>,
    /// Focal length, in millimetres, and its 35 mm equivalent when the camera
    /// states one — a focal length means nothing without the sensor it was
    /// measured on.
    pub focal_length: Option<f64>,
    pub equivalent: Option<u32>,
    pub taken: Option<Taken>,
}

/// When a picture was taken.
///
/// The text as the camera wrote it is what a person reads; the moment is what
/// two pictures are measured against each other by. A camera writing a date
/// this build cannot parse still has it shown, and simply cannot be measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Taken {
    /// The date as written, `2026:08:28 14:03:11`.
    raw: String,
    /// The fraction of the second, as its digits: `"20"` is two tenths.
    ///
    /// Stored as the digits rather than as a number because it is shown as
    /// written, and `.20` read back from a float is not always `.20`.
    subsec: Option<String>,
    /// The offset from UTC the camera stated, in minutes.
    offset: Option<i32>,
}

impl Taken {
    /// The date the way a person reads it: dashes rather than the standard's
    /// colons, and the fraction of a second when the camera kept one — a
    /// burst of ten frames a second lives inside one second, and without it
    /// every frame of the burst reads as taken at the same moment.
    pub fn text(&self) -> String {
        let date = readable_date(&self.raw);
        match &self.subsec {
            Some(subsec) => format!("{date}.{subsec}"),
            None => date,
        }
    }

    /// The time of day alone, for a table where the date is the same on both
    /// sides and would only push the time out of sight.
    pub fn time_text(&self) -> String {
        let text = self.text();
        match text.split_once(' ') {
            Some((_, time)) => time.to_string(),
            None => text,
        }
    }

    /// The calendar day, as written, to tell whether two moments share one.
    pub fn day(&self) -> &str {
        self.raw.split_once(' ').map_or(self.raw.as_str(), |(day, _)| day)
    }

    /// Seconds from `self` to `later`, when both can be read as moments.
    ///
    /// The offsets are honoured when both pictures state one — a camera
    /// carried across a time zone between two frames writes two different
    /// clocks — and ignored when either does not, because a clock with no
    /// offset is read as the same local time the other is in, which is the
    /// only reading that does not invent a difference.
    pub fn seconds_until(&self, later: &Taken) -> Option<f64> {
        let (from, to) = (self.moment()?, later.moment()?);
        let shift = match (self.offset, later.offset) {
            (Some(from_offset), Some(to_offset)) => f64::from(from_offset - to_offset) * 60.0,
            _ => 0.0,
        };
        Some(to - from + shift)
    }

    /// Seconds since 1970, in the camera's own clock, with the fraction.
    fn moment(&self) -> Option<f64> {
        let seconds = civil_seconds(&self.raw)?;
        let fraction = match &self.subsec {
            Some(digits) => format!("0.{digits}").parse::<f64>().ok()?,
            None => 0.0,
        };
        Some(seconds as f64 + fraction)
    }
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
        shot: shot(&exif),
        location: location(&exif),
    }
}

fn read_exif(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = Cursor::new(bytes);
    exif::Reader::new().read_from_container(&mut cursor).ok()
}

/// The fields worth reading, as values.
fn shot(exif: &exif::Exif) -> Shot {
    let make = text(exif, exif::Tag::Make);
    let model = text(exif, exif::Tag::Model);
    let camera = match (make, model) {
        // Most makers repeat themselves — "NIKON CORPORATION" then "NIKON
        // D850" — and printing both reads as a stutter.
        (Some(make), Some(model)) => Some(if model.starts_with(&make) { model } else { format!("{make} {model}") }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };

    // The original moment first, and the file's own date only when the camera
    // wrote nothing better; each has its own fraction and offset, and taking
    // the fraction of one with the date of the other would be a moment
    // neither of them states.
    let taken = match text(exif, exif::Tag::DateTimeOriginal) {
        Some(raw) => Some(Taken {
            raw,
            subsec: subsec(exif, exif::Tag::SubSecTimeOriginal),
            offset: offset(exif, exif::Tag::OffsetTimeOriginal),
        }),
        None => text(exif, exif::Tag::DateTime).map(|raw| Taken {
            raw,
            subsec: subsec(exif, exif::Tag::SubSecTime),
            offset: offset(exif, exif::Tag::OffsetTime),
        }),
    };

    Shot {
        camera,
        lens: text(exif, exif::Tag::LensModel),
        // A zero exposure is not a picture; it is a file saying nothing.
        exposure: rational(exif, exif::Tag::ExposureTime).filter(|value| *value > 0.0),
        aperture: rational(exif, exif::Tag::FNumber).filter(|value| *value > 0.0),
        iso: uint(exif, exif::Tag::PhotographicSensitivity),
        compensation: rational(exif, exif::Tag::ExposureBiasValue).filter(|value| value.is_finite()),
        focal_length: rational(exif, exif::Tag::FocalLength).filter(|value| *value > 0.0),
        equivalent: uint(exif, exif::Tag::FocalLengthIn35mmFilm).filter(|value| *value > 0),
        taken,
    }
}

/// Shutter speed, as a photographer says it: `1/250 s` below a second and
/// `2 s` above, rather than the raw rational either way.
pub fn exposure_text(seconds: f64) -> String {
    if seconds >= 1.0 {
        return format!("{} s", trim(seconds));
    }
    // A camera stores 1/250 as 0.004, and the reciprocal is what is written on
    // the dial. Rounded, because 1/249.99 is the same picture.
    format!("1/{} s", (1.0 / seconds).round() as u64)
}

pub fn aperture_text(f_number: f64) -> String {
    format!("f/{}", trim(f_number))
}

pub fn focal_text(millimetres: f64, equivalent: Option<u32>) -> String {
    match equivalent {
        Some(equivalent) => format!("{} mm ({equivalent} mm eq.)", trim(millimetres)),
        None => format!("{} mm", trim(millimetres)),
    }
}

/// Compensation with its sign, always: `+0.7 EV` and `−1 EV` are dialled in
/// opposite directions, and a bare `0.7` does not say which.
pub fn compensation_text(stops: f64) -> String {
    let rounded = (stops * 10.0).round() / 10.0;
    if rounded == 0.0 {
        return "0 EV".to_string();
    }
    let sign = if rounded > 0.0 { "+" } else { "\u{2212}" };
    format!("{sign}{} EV", trim(rounded.abs()))
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

/// Seconds since 1970 for a date written the standard's way, read as UTC.
///
/// `None` for anything that is not a real date: cameras with no clock set
/// write `0000:00:00 00:00:00`, and measuring a burst against that would
/// report a difference of two thousand years.
fn civil_seconds(raw: &str) -> Option<i64> {
    let (date, time) = raw.trim().split_once(' ')?;
    let mut date = date.split(':').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut time = time.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);

    // A leap second is written as :60, so the range allows it.
    let valid = (1..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..=60).contains(&second);
    if !valid {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar.
///
/// Howard Hinnant's algorithm: the year is shifted to start in March so the
/// leap day falls at its end, and counted in 400-year eras, which repeat
/// exactly. Twenty lines rather than a date crate for one subtraction.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The fraction of a second, as the digits the camera wrote.
///
/// Some cameras pad the field with spaces or write nothing but them; only the
/// leading digits are the fraction, and none at all is no fraction.
fn subsec(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let raw = text(exif, tag)?;
    let digits: String = raw.chars().take_while(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

/// An offset from UTC, written `+03:00` or `-04:30`, in minutes.
fn offset(exif: &exif::Exif, tag: exif::Tag) -> Option<i32> {
    parse_offset(&text(exif, tag)?)
}

fn parse_offset(raw: &str) -> Option<i32> {
    let raw = raw.trim();
    let (sign, rest) = match raw.as_bytes().first()? {
        b'+' => (1, &raw[1..]),
        b'-' => (-1, &raw[1..]),
        _ => return None,
    };
    let (hours, minutes) = rest.split_once(':')?;
    let (hours, minutes) = (hours.parse::<i32>().ok()?, minutes.parse::<i32>().ok()?);
    ((0..=14).contains(&hours) && (0..60).contains(&minutes)).then_some(sign * (hours * 60 + minutes))
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

/// A measured value, as a number.
///
/// The standard says rational, and cameras write rationals. Software that
/// rewrites a file sometimes writes a float instead — Pillow does, for a Python
/// float — and refusing it would leave the exposure out of the panel and out
/// of a comparison for a reason no person looking at the picture could guess.
/// A value that is not finite is not a measurement.
fn rational(exif: &exif::Exif, tag: exif::Tag) -> Option<f64> {
    let field = exif.get_field(tag, exif::In::PRIMARY)?;
    let value = match field.value {
        exif::Value::Rational(ref values) => values.first().map(|value| value.to_f64()),
        exif::Value::SRational(ref values) => values.first().map(|value| value.to_f64()),
        exif::Value::Double(ref values) => values.first().copied(),
        exif::Value::Float(ref values) => values.first().map(|value| f64::from(*value)),
        _ => None,
    }?;
    value.is_finite().then_some(value)
}

/// Drop the decimals a number does not need: `2.8` stays, `35.0` becomes `35`.
pub fn trim(value: f64) -> String {
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

/// A moment built from its parts, for the comparison's tests: the fields are
/// private because a `Taken` is otherwise only ever read from a file.
#[cfg(test)]
pub fn taken(raw: &str, subsec: Option<&str>, offset: Option<i32>) -> Taken {
    Taken {
        raw: raw.to_string(),
        subsec: subsec.map(str::to_string),
        offset,
    }
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
        SRational(Vec<(i32, i32)>),
        Double(f64),
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
                Value::SRational(parts) => {
                    let mut bytes = Vec::new();
                    for (numerator, denominator) in parts {
                        bytes.extend_from_slice(&numerator.to_le_bytes());
                        bytes.extend_from_slice(&denominator.to_le_bytes());
                    }
                    (10, parts.len() as u32, bytes)
                }
                Value::Double(value) => (12, 1, value.to_le_bytes().to_vec()),
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
        metadata.camera().iter().find(|entry| entry.label == label).map(|entry| entry.value.clone())
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
        assert!(metadata.camera().is_empty());
        assert!(metadata.location.is_none());

        // And so does something that is not an image at all.
        let metadata = read(b"not an image");
        assert!(metadata.camera().is_empty());
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

    /// A burst of ten frames a second lives inside one second, so the
    /// fraction the camera keeps beside the date is what tells its frames
    /// apart — both on screen and when two of them are measured.
    #[test]
    fn a_burst_frame_keeps_its_fraction_of_a_second() {
        let frame = |subsec: &'static str| {
            read(&jpeg_with(
                &Exif::default()
                    .sub(0x9003, Value::Ascii("2026:08:28 14:03:11"))
                    .sub(0x9291, Value::Ascii(subsec))
                    .build(),
            ))
        };
        let first = frame("20");
        let second = frame("60");

        assert_eq!(value_of(&first, "Taken").as_deref(), Some("2026-08-28 14:03:11.20"));
        let first = first.shot.taken.expect("a moment");
        let second = second.shot.taken.expect("a moment");
        let gap = first.seconds_until(&second).expect("both are real dates");
        assert!((gap - 0.4).abs() < 1e-6, "two frames four tenths apart measured {gap} s");
        assert_eq!(first.time_text(), "14:03:11.20");
    }

    /// A camera with its clock never set writes a date that is not one, and
    /// measuring against it would report two thousand years.
    #[test]
    fn a_date_that_is_not_one_is_shown_but_not_measured() {
        let unset = taken("0000:00:00 00:00:00", None, None);
        let real = taken("2026:08:28 14:03:11", None, None);
        assert_eq!(unset.seconds_until(&real), None);
        assert_eq!(unset.text(), "0000-00-00 00:00:00");
        assert_eq!(
            taken("2026:13:01 00:00:00", None, None).seconds_until(&real),
            None,
            "a thirteenth month was measured"
        );
    }

    /// The calendar arithmetic, against days anyone can check: the epoch, a
    /// leap day, and the day after it.
    #[test]
    fn days_are_counted_across_months_and_leap_years() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2, "2000 was a leap year");
        assert_eq!(days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28), 1, "1900 was not");
        assert_eq!(days_from_civil(2026, 1, 1), 20454);

        let midnight = taken("2026:08:28 23:59:59", None, None);
        let next_day = taken("2026:08:29 00:00:01", None, None);
        assert_eq!(midnight.seconds_until(&next_day), Some(2.0), "two seconds across midnight");
    }

    /// Two clocks in two zones are compared as the same instant would be;
    /// a clock with no zone is read as the local time the other is in.
    #[test]
    fn stated_offsets_are_honoured_and_missing_ones_are_not_invented() {
        let home = taken("2026:08:28 14:00:00", None, Some(-180));
        let abroad = taken("2026:08:28 20:00:00", None, Some(180));
        assert_eq!(home.seconds_until(&abroad), Some(0.0), "the same instant in two zones read as six hours apart");

        let unzoned = taken("2026:08:28 20:00:00", None, None);
        assert_eq!(home.seconds_until(&unzoned), Some(6.0 * 3600.0));

        assert_eq!(parse_offset("+03:00"), Some(180));
        assert_eq!(parse_offset("-04:30"), Some(-270));
        assert_eq!(parse_offset("03:00"), None, "an offset without a sign");
        assert_eq!(parse_offset("+25:00"), None);
    }

    /// Written as floats rather than rationals — what Pillow does with a
    /// Python float — the exposure still reads. Found by a live comparison of
    /// Pillow-made frames, where the shutter, the aperture and the focal
    /// length were simply missing from the table.
    #[test]
    fn a_value_written_as_a_float_still_reads() {
        let rewritten = read(&jpeg_with(
            &Exif::default()
                .sub(0x829A, Value::Double(0.004))
                .sub(0x829D, Value::Double(2.8))
                .sub(0x920A, Value::Double(35.0))
                .build(),
        ));
        assert_eq!(value_of(&rewritten, "Exposure").as_deref(), Some("1/250 s"));
        assert_eq!(value_of(&rewritten, "Aperture").as_deref(), Some("f/2.8"));
        assert_eq!(value_of(&rewritten, "Focal length").as_deref(), Some("35 mm"));

        let nonsense = read(&jpeg_with(&Exif::default().sub(0x829D, Value::Double(f64::NAN)).build()));
        assert_eq!(value_of(&nonsense, "Aperture"), None, "a value that is not a number was shown");
    }

    /// Compensation is shown only when it was dialled in, and always with its
    /// sign — but kept as a value at zero, because a bracketed series is told
    /// apart by it and the frame at zero is one of the three.
    #[test]
    fn compensation_is_signed_and_hidden_at_zero() {
        let dialled = read(&jpeg_with(&Exif::default().sub(0x9204, Value::SRational(vec![(-2, 3)])).build()));
        assert_eq!(value_of(&dialled, "Compensation").as_deref(), Some("\u{2212}0.7 EV"));

        let level = read(&jpeg_with(&Exif::default().sub(0x9204, Value::SRational(vec![(0, 1)])).build()));
        assert_eq!(value_of(&level, "Compensation"), None, "a level exposure was given a line");
        assert_eq!(level.shot.compensation, Some(0.0), "the zero was dropped from the values");

        assert_eq!(compensation_text(1.0), "+1 EV");
    }
}
