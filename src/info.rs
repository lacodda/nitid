//! `nitid info`: what a picture says about itself, in a terminal.
//!
//! The Info panel (`I`) and the colour passport (`K`) answer this inside the
//! window. This answers it where a person at a prompt, or a script, can read
//! it: the same decode, the same EXIF reader, the same words for a shutter
//! speed — so the two never disagree about a file.
//!
//! Text for a person, `--json` for a script. Both are needed at once and by
//! different readers, which is the one case a second way of doing a thing is
//! not a setting in disguise: text a script parses breaks the day a label is
//! reworded, and JSON in a terminal is a wall.
//!
//! Nothing here depends on the display: the passport's "between them" is a
//! question about this screen, and a file's description is not.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::batch;
use crate::color::Described;
use crate::cull::Mark;
use crate::format::Format;
use crate::image_source::{Depth, Orientation};
use crate::json::Value;
use crate::metadata::Metadata;

/// What `nitid info` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    pub paths: Vec<PathBuf>,
    pub json: bool,
}

impl Asked {
    /// Read what followed `nitid info`.
    pub fn parse(args: impl Iterator<Item = OsString>) -> Result<Self> {
        let mut paths = Vec::new();
        let mut json = false;
        for arg in args {
            match arg.to_str() {
                Some("--json") => json = true,
                Some(flag) if flag.starts_with("--") => bail!("{flag} is not something info takes"),
                _ => paths.push(PathBuf::from(arg)),
            }
        }
        if paths.is_empty() {
            bail!("info needs a picture, a folder or a pattern such as *.jpg");
        }
        Ok(Self { paths, json })
    }
}

/// Print what each picture named says about itself.
pub fn command(asked: Asked) -> Result<()> {
    let named = batch::named(&asked.paths);
    let mut failed = !named.missing.is_empty();

    if asked.json {
        let mut entries: Vec<Value> = named.missing.iter().map(|(path, why)| unreadable(path, why)).collect();
        batch::each(&named.files, read, |path, result| match result {
            Ok(report) => entries.push(report.json()),
            Err(error) => {
                failed = true;
                entries.push(unreadable(path, &format!("{error:#}")));
            }
        });
        print!("{}", Value::List(entries).render());
    } else {
        for (path, why) in &named.missing {
            eprintln!("nitid: {}: {why}", path.display());
        }
        let mut first = true;
        batch::each(&named.files, read, |path, result| match result {
            Ok(report) => {
                if !first {
                    println!();
                }
                first = false;
                print!("{}", report.text());
            }
            Err(error) => {
                failed = true;
                eprintln!("nitid: {}: {error:#}", path.display());
            }
        });
    }

    if failed { Err(batch::Reported.into()) } else { Ok(()) }
}

/// One entry for a file that could not be read: the path, so a script can
/// pair it with what it asked, and why.
fn unreadable(path: &Path, why: &str) -> Value {
    Value::Object(vec![("path", Value::Text(path.display().to_string())), ("error", Value::Text(why.to_string()))])
}

/// Everything said about one picture.
struct Report {
    path: PathBuf,
    bytes: u64,
    format: Format,
    /// The size as shown, after the orientation.
    size: (u32, u32),
    depth: Depth,
    orientation: Orientation,
    frames: usize,
    mark: Mark,
    /// `None` for an untagged file.
    colour: Option<Described>,
    /// The code points a HEIC states its colour by instead of a profile.
    ///
    /// Its decoder reads them and hands over sRGB, so there is no profile to
    /// describe — but the file did say something, and "says nothing" would be
    /// wrong about it.
    codes: Option<crate::isobmff::Cicp>,
    caveat: Option<String>,
    metadata: Metadata,
}

/// Decode the picture and gather what it says.
///
/// A full decode, not a look at the header: the colour of an AVIF or a JPEG XL
/// is read by its decoder on the way to the pixels (see `color::raw_profile`),
/// and an answer that differed by format in what it could say would be worse
/// than a slower one.
fn read(path: &Path) -> Result<Report> {
    let bytes = std::fs::read(path).context("reading the file")?;
    let loaded = crate::image_source::decode(&bytes)?;
    Ok(Report {
        path: path.to_path_buf(),
        bytes: bytes.len() as u64,
        format: loaded.format,
        size: loaded.display_size(),
        depth: loaded.image.depth,
        orientation: loaded.orientation,
        frames: loaded.animation.as_ref().map_or(1, |animation| animation.frames.len()),
        mark: loaded.mark,
        colour: loaded.profile.as_ref().map(crate::color::describe),
        codes: match (loaded.format, &loaded.profile) {
            (Format::Heic, None) => crate::isobmff::colour_box(&bytes).and_then(|colour| colour.cicp),
            _ => None,
        },
        caveat: loaded.caveat,
        metadata: loaded.metadata,
    })
}

/// How wide the label column is: the longest label and two spaces.
const LABEL: usize = 13;

impl Report {
    /// The picture as a block of lines: its name, then the sections the Info
    /// panel has, labelled as the panel labels them.
    ///
    /// The size is `4032 x 3024`, not the panel's multiplication sign: it is
    /// the line a script is likeliest to pick out, and a console still
    /// decodes a program's output in its own code page. The camera's lines
    /// are the panel's own, word for word.
    fn text(&self) -> String {
        let mut out = format!("{}\n", self.path.display());
        let mut row = |label: &str, value: &str| out.push_str(&format!("  {label:<LABEL$}{value}\n"));

        row("Size", &format!("{} x {}", self.size.0, self.size.1));
        row("Format", self.format.name());
        row("Depth", depth_text(self.depth));
        row("File", &crate::interface::file_size(self.bytes));
        if let Some(words) = self.orientation.words() {
            row("Orientation", &format!("turned {words} (EXIF {})", self.orientation.to_exif()));
        }
        if self.frames > 1 {
            row("Frames", &self.frames.to_string());
        }
        match self.mark {
            Mark::Keep => row("Mark", "kept"),
            Mark::Reject => row("Mark", "rejected"),
            Mark::Unmarked => {}
        }

        out.push_str("\n  Colour\n");
        let mut row = |label: &str, value: &str| out.push_str(&format!("  {label:<LABEL$}{value}\n"));
        match (&self.colour, self.codes) {
            (None, Some(codes)) => {
                row("Profile", "none: the colour is stated as code points");
                let named = |name: Option<&str>, what: &str| name.map_or_else(|| format!("unnamed {what}"), |name| format!("{name} {what}"));
                row(
                    "Codes",
                    &format!(
                        "{}/{}/{}: {}, {}",
                        codes.primaries,
                        codes.transfer,
                        codes.matrix,
                        named(crate::color::primaries_code_name(codes.primaries), "primaries"),
                        crate::color::transfer_code_name(codes.transfer).map_or_else(|| "unnamed transfer".to_string(), str::to_string),
                    ),
                );
            }
            // ADR 0005: said as what it means, not as an absence.
            (None, None) => row("Profile", "none: the file does not say what its colours mean"),
            (Some(colour), _) => {
                row("Profile", colour.name.as_deref().unwrap_or("unnamed"));
                row("Primaries", colour.primaries.unwrap_or("its own"));
                if let Some(transfer) = &colour.transfer {
                    row("Transfer", transfer);
                }
                if colour.hdr {
                    row("Range", "high dynamic range");
                }
                row(
                    "ICC",
                    &format!(
                        "version {}, {} profile for {}, {} intent",
                        colour.version, colour.class, colour.space, colour.intent
                    ),
                );
            }
        }
        if let Some(caveat) = &self.caveat {
            row("Caveat", caveat);
        }

        let camera = self.metadata.camera();
        if !camera.is_empty() {
            out.push_str("\n  Camera\n");
            for entry in camera {
                out.push_str(&format!("  {:<LABEL$}{}\n", entry.label, entry.value));
            }
        }
        if let Some(place) = self.metadata.location {
            out.push_str("\n  Place\n");
            out.push_str(&format!("  {:<LABEL$}{}\n", "Coordinates", place.as_text()));
        }
        out
    }

    /// The same facts as values a script reads without parsing prose:
    /// seconds rather than `1/250 s`, the EXIF number for the orientation,
    /// `null` for what the file does not say.
    fn json(&self) -> Value {
        let shot = &self.metadata.shot;
        Value::Object(vec![
            ("path", Value::Text(self.path.display().to_string())),
            ("format", Value::Text(self.format.name().to_string())),
            ("width", Value::Count(self.size.0.into())),
            ("height", Value::Count(self.size.1.into())),
            (
                "depth",
                Value::Count(match self.depth {
                    Depth::Eight => 8,
                    Depth::Sixteen => 16,
                }),
            ),
            ("bytes", Value::Count(self.bytes)),
            ("orientation", Value::Count(self.orientation.to_exif().into())),
            ("frames", Value::Count(self.frames as u64)),
            (
                "mark",
                match self.mark {
                    Mark::Keep => Value::Text("kept".into()),
                    Mark::Reject => Value::Text("rejected".into()),
                    Mark::Unmarked => Value::Null,
                },
            ),
            (
                "colour",
                match &self.colour {
                    None => Value::Null,
                    Some(colour) => Value::Object(vec![
                        ("profile", Value::text(colour.name.clone())),
                        ("primaries", Value::text(colour.primaries)),
                        ("transfer", Value::text(colour.transfer.clone())),
                        ("hdr", Value::Bool(colour.hdr)),
                        ("version", Value::Text(colour.version.into())),
                        ("class", Value::Text(colour.class.into())),
                        ("space", Value::Text(colour.space.into())),
                        ("intent", Value::Text(colour.intent.into())),
                    ]),
                },
            ),
            (
                "codes",
                match self.codes {
                    None => Value::Null,
                    Some(codes) => Value::Object(vec![
                        ("primaries", Value::Count(codes.primaries.into())),
                        ("transfer", Value::Count(codes.transfer.into())),
                        ("matrix", Value::Count(codes.matrix.into())),
                    ]),
                },
            ),
            ("caveat", Value::text(self.caveat.clone())),
            (
                "camera",
                Value::Object(vec![
                    ("camera", Value::text(shot.camera.clone())),
                    ("lens", Value::text(shot.lens.clone())),
                    ("exposure", Value::number(shot.exposure)),
                    ("aperture", Value::number(shot.aperture)),
                    ("iso", Value::count(shot.iso.map(u64::from))),
                    ("compensation", Value::number(shot.compensation)),
                    ("focal_length", Value::number(shot.focal_length)),
                    ("focal_length_35mm", Value::count(shot.equivalent.map(u64::from))),
                    ("taken", Value::text(shot.taken.as_ref().map(crate::metadata::Taken::text))),
                ]),
            ),
            (
                "place",
                match self.metadata.location {
                    None => Value::Null,
                    Some(place) => Value::Object(vec![("latitude", Value::Number(place.latitude)), ("longitude", Value::Number(place.longitude))]),
                },
            ),
        ])
    }
}

fn depth_text(depth: Depth) -> &'static str {
    match depth {
        Depth::Eight => "8-bit",
        Depth::Sixteen => "16-bit",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Asked> {
        Asked::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn the_arguments_are_read() {
        assert_eq!(
            parse(&["a.jpg", "--json", "*.png"]).unwrap(),
            Asked {
                paths: vec![PathBuf::from("a.jpg"), PathBuf::from("*.png")],
                json: true,
            }
        );
        assert!(parse(&[]).is_err(), "info with nothing to describe was taken");
        assert!(parse(&["a.jpg", "--jsno"]).is_err(), "a mistyped flag was taken as a file");
    }

    /// A tagged, turned photograph with a camera and a place: every section
    /// the text has, each from the field it should come from.
    fn report() -> Report {
        let mut metadata = Metadata::default();
        metadata.shot.camera = Some("Synthetic Camera".into());
        metadata.shot.exposure = Some(0.004);
        metadata.shot.iso = Some(200);
        metadata.location = Some(crate::metadata::Location {
            latitude: 12.5,
            longitude: -45.25,
        });
        Report {
            path: PathBuf::from("pictures").join("shot.heic"),
            bytes: 3 * 1024 * 1024,
            format: Format::Heic,
            size: (3024, 4032),
            depth: Depth::Eight,
            orientation: Orientation::Rotate90,
            frames: 1,
            mark: Mark::Reject,
            colour: Some(crate::color::describe(&moxcms::ColorProfile::new_display_p3())),
            codes: None,
            caveat: None,
            metadata,
        }
    }

    #[test]
    fn the_text_says_what_the_panel_says() {
        let text = report().text();
        for line in [
            "  Size         3024 x 4032",
            "  Format       HEIC",
            "  Depth        8-bit",
            "  File         3.0 MB",
            "  Orientation  turned a quarter clockwise (EXIF 6)",
            "  Mark         rejected",
            "  Primaries    Display P3",
            "  Transfer     sRGB curve",
            "  Camera       Synthetic Camera",
            "  Exposure     1/250 s",
            "  ISO          200",
            "  Coordinates  12.500000, -45.250000",
        ] {
            assert!(text.lines().any(|candidate| candidate == line), "{line:?} is missing from:\n{text}");
        }
        assert!(text.starts_with(&format!("{}\n", PathBuf::from("pictures").join("shot.heic").display())));
    }

    /// An untagged file says what that means, an upright one says nothing
    /// about turning, and a file without EXIF has no camera section.
    #[test]
    fn what_a_file_does_not_say_is_said_or_left_out() {
        let mut report = report();
        report.colour = None;
        report.orientation = Orientation::Normal;
        report.metadata = Metadata::default();
        report.mark = Mark::Unmarked;
        let text = report.text();
        assert!(text.contains("none: the file does not say what its colours mean"), "{text}");
        for absent in ["Orientation", "Camera\n", "Place", "Mark"] {
            assert!(!text.contains(absent), "{absent:?} appeared in:\n{text}");
        }
    }

    /// A HEIC that states its colour as code points is said to, by name, and
    /// not called silent.
    #[test]
    fn code_points_are_named_rather_than_called_silence() {
        let mut report = report();
        report.colour = None;
        report.codes = Some(crate::isobmff::Cicp {
            primaries: 12,
            transfer: 13,
            matrix: 6,
        });
        let text = report.text();
        assert!(text.contains("  Codes        12/13/6: Display P3 primaries, sRGB curve\n"), "{text}");
        assert!(!text.contains("does not say"), "{text}");
        assert!(
            report.json().render().contains("\"codes\": {\n    \"primaries\": 12,"),
            "{}",
            report.json().render()
        );
    }

    /// The JSON carries values, not prose, and `null` for what is not said.
    #[test]
    fn the_json_carries_values() {
        let json = report().json().render();
        for field in [
            "\"width\": 3024",
            "\"height\": 4032",
            "\"orientation\": 6",
            "\"exposure\": 0.004",
            "\"iso\": 200",
            "\"lens\": null",
            "\"mark\": \"rejected\"",
            "\"primaries\": \"Display P3\"",
            "\"latitude\": 12.5",
        ] {
            assert!(json.contains(field), "{field} is missing from:\n{json}");
        }
    }
}
