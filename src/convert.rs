//! `nitid convert`: pictures written again in another format.
//!
//! Every picture goes through the export the save box uses (`export`), so a
//! converted file is what `Ctrl+Shift+S` would have written: the profile kept
//! and written, or the colour baked into sRGB on `--srgb`; an HDR picture as
//! a standard-range screen shows it; turned the way it is shown.
//!
//! Where it differs from a save is in what it is for. A save writes a new
//! picture of what was on screen, and writes no EXIF (ADR 0028). A conversion
//! is the same photograph in another container — the reason to run it over a
//! phone's HEICs is a program that cannot open them — so it keeps what the
//! photograph says about itself: the EXIF block, with its orientation set
//! upright because the pixels are; the mark it was given in a cull; and the
//! time the file was last written, so a folder sorted by date, and the
//! viewer's own "newest", still put the pictures where they were.
//!
//! Nothing is replaced. A name that is already taken is left alone and said
//! so, which also makes a second run over the same folder pick up where an
//! interrupted one stopped.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::batch;
use crate::export::{Colour, Target};

/// What `nitid convert` was asked.
#[derive(Debug, PartialEq, Eq)]
pub struct Asked {
    pub paths: Vec<PathBuf>,
    pub target: Target,
    /// JPEG and WebP quality, 1-100.
    pub quality: u8,
    /// Convert the colour into sRGB rather than keep the file's profile.
    pub bake: bool,
    /// Where the new files go; beside each picture when `None`.
    pub out: Option<PathBuf>,
}

impl Asked {
    /// Read what followed `nitid convert`.
    pub fn parse(args: impl Iterator<Item = OsString>) -> Result<Self> {
        let mut paths = Vec::new();
        let (mut target, mut quality, mut bake, mut out) = (None, crate::export::QUALITY, false, None);

        let mut args = args;
        while let Some(arg) = args.next() {
            let mut value = |flag: &str| args.next().with_context(|| format!("{flag} needs a value"));
            match arg.to_str() {
                Some("--to") => {
                    let text = value("--to")?.to_string_lossy().into_owned();
                    target = Some(Target::from_extension(&text).with_context(|| format!("{text} is not a format convert writes: jpeg, png or webp"))?);
                }
                Some("--quality") => {
                    let text = value("--quality")?.to_string_lossy().into_owned();
                    quality = text
                        .parse::<u8>()
                        .ok()
                        .filter(|quality| (1..=100).contains(quality))
                        .with_context(|| format!("{text} is not a quality: it is a number from 1 to 100"))?;
                }
                Some("--srgb") => bake = true,
                Some("--out") => out = Some(PathBuf::from(value("--out")?)),
                Some(flag) if flag.starts_with("--") => bail!("{flag} is not something convert takes"),
                _ => paths.push(PathBuf::from(arg)),
            }
        }

        let Some(target) = target else {
            bail!("convert needs --to jpeg, png or webp");
        };
        if paths.is_empty() {
            bail!("convert needs a picture, a folder or a pattern such as *.heic");
        }
        Ok(Self {
            paths,
            target,
            quality,
            bake,
            out,
        })
    }
}

/// What became of one picture.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Written, and what it lost on the way, in the save box's words.
    Written { to: PathBuf, warnings: Vec<String> },
    /// Not written, because the name is taken.
    Left { to: PathBuf, why: &'static str },
}

/// Convert every picture named.
pub fn command(asked: Asked) -> Result<()> {
    let named = batch::named(&asked.paths);
    for (path, why) in &named.missing {
        eprintln!("nitid: {}: {why}", path.display());
    }
    if let Some(out) = &asked.out {
        std::fs::create_dir_all(out).with_context(|| format!("making {}", out.display()))?;
    }

    let (mut written, mut left, mut failed) = (0usize, 0usize, named.missing.len());
    batch::each(
        &named.files,
        |source| convert(source, &asked),
        |source, outcome| match outcome {
            Ok(Outcome::Written { to, warnings }) => {
                written += 1;
                println!("{} -> {}", source.display(), to.display());
                for warning in warnings {
                    println!("  {warning}");
                }
            }
            Ok(Outcome::Left { to, why }) => {
                left += 1;
                println!("{} -> {}: {why}, left alone", source.display(), to.display());
            }
            Err(error) => {
                failed += 1;
                eprintln!("nitid: {}: {error:#}", source.display());
            }
        },
    );

    let total = named.files.len() + named.missing.len();
    if total > 1 {
        println!("{}", summary(written, left, failed, total));
    }
    if failed > 0 { Err(batch::Reported.into()) } else { Ok(()) }
}

/// The last line of a run over several pictures.
fn summary(written: usize, left: usize, failed: usize, total: usize) -> String {
    let mut said = format!("converted {written} of {total}");
    if left > 0 {
        said.push_str(&format!(", {left} already there"));
    }
    if failed > 0 {
        said.push_str(&format!(", {failed} failed"));
    }
    said
}

/// Where a picture converted to `target` is written: the folder asked for,
/// or the picture's own, under its name with the new extension.
///
/// The extension is added to the stem rather than swapped with
/// `with_extension`, which would read `shot.2026` as a stem `shot` and write
/// `shot.jpg` for both `shot.2026.heic` and `shot.2027.heic`.
fn destination(source: &Path, target: Target, out: Option<&Path>) -> Result<PathBuf> {
    let stem = source.file_stem().with_context(|| format!("{} has no file name", source.display()))?;
    let mut name = stem.to_os_string();
    name.push(".");
    name.push(target.extension());
    let folder = out.or_else(|| source.parent()).unwrap_or(Path::new(""));
    Ok(folder.join(name))
}

/// Convert one picture.
fn convert(source: &Path, asked: &Asked) -> Result<Outcome> {
    let to = destination(source, asked.target, asked.out.as_deref())?;
    // Checked before the decode, which is the expensive part: a second run
    // over a folder skips what the first one finished without reading it.
    if same_file(source, &to) {
        return Ok(Outcome::Left {
            to,
            why: "it is the picture itself",
        });
    }
    if to.exists() {
        return Ok(Outcome::Left { to, why: "already there" });
    }

    let bytes = std::fs::read(source).context("reading the file")?;
    let modified = std::fs::metadata(source).and_then(|metadata| metadata.modified()).ok();
    let loaded = crate::image_source::decode(&bytes)?;
    let exif = crate::metadata::exif_block_upright(&bytes);

    let request = crate::export::Request {
        image: &loaded.image,
        profile: loaded.profile.as_ref(),
        target: asked.target,
        colour: if asked.bake { Colour::BakeToSrgb } else { Colour::KeepProfile },
        quality: asked.quality,
        caveat: loaded.caveat.as_deref(),
        orientation: loaded.orientation,
        exif: exif.as_deref(),
    };
    let mut warnings = crate::export::warnings(&request);
    if let Some(animation) = &loaded.animation {
        warnings.push(format!("only the first of its {} frames is written", animation.frames.len()));
    }
    let encoded = crate::export::encode(&request)?;

    if !write_new(&to, &encoded)? {
        // Another picture of the same name, converted at the same moment on
        // another thread, took the name between the check and the write.
        return Ok(Outcome::Left { to, why: "already there" });
    }

    // A keep rides in the EXIF block and arrives with it; a reject lives in
    // XMP, which is not carried, and is written now. A format that cannot
    // hold the mark refuses and leaves the file as it was (`cull::write`).
    if crate::cull::from_bytes(&encoded) != loaded.mark
        && let Err(error) = crate::cull::write(&to, loaded.mark)
    {
        warnings.push(format!("the mark was not carried: {error:#}"));
    }

    // Last, after anything that rewrites the file.
    if let Some(modified) = modified {
        let stamped = std::fs::File::options().write(true).open(&to).and_then(|file| file.set_modified(modified));
        if let Err(error) = stamped {
            warnings.push(format!("the time it was taken from could not be kept: {error}"));
        }
    }

    Ok(Outcome::Written { to, warnings })
}

/// Whether two paths are one file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Write `bytes` to a file that must not exist yet.
///
/// `false` when it does: the check before the decode is a courtesy, and this
/// is the one that holds, because the open itself refuses a name that is
/// taken. A write that fails part-way removes what it left, so a disk that
/// fills up does not leave a picture that opens as half a picture.
fn write_new(path: &Path, bytes: &[u8]) -> Result<bool> {
    let mut file = match std::fs::File::options().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("creating {}", path.display())),
    };
    if let Err(error) = file.write_all(bytes).and_then(|()| file.flush()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error).with_context(|| format!("writing {}", path.display()));
    }
    Ok(true)
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
            parse(&["*.heic", "--to", "jpeg"]).unwrap(),
            Asked {
                paths: vec![PathBuf::from("*.heic")],
                target: Target::Jpeg,
                quality: crate::export::QUALITY,
                bake: false,
                out: None,
            }
        );
        assert_eq!(
            parse(&["--to", "webp", "a.png", "--quality", "75", "--srgb", "--out", "sent", "b.png"]).unwrap(),
            Asked {
                paths: vec![PathBuf::from("a.png"), PathBuf::from("b.png")],
                target: Target::WebP,
                quality: 75,
                bake: true,
                out: Some(PathBuf::from("sent")),
            }
        );
    }

    #[test]
    fn a_mistake_is_said_before_anything_is_written() {
        for (args, said) in [
            (&["a.heic"][..], "needs --to"),
            (&["--to", "jpeg"][..], "needs a picture"),
            (&["a.heic", "--to", "gif"][..], "not a format convert writes"),
            (&["a.heic", "--to"][..], "--to needs a value"),
            (&["a.heic", "--to", "png", "--quality", "0"][..], "not a quality"),
            (&["a.heic", "--to", "png", "--quality", "101"][..], "not a quality"),
            (&["a.heic", "--to", "png", "--overwrite"][..], "not something convert takes"),
        ] {
            let error = parse(args).expect_err(&format!("{args:?} was taken"));
            assert!(format!("{error:#}").contains(said), "{args:?}: {error:#}");
        }
    }

    /// The new name is the old one with the new extension, beside it or in
    /// the folder asked for — and a stem with a dot in it keeps the dot.
    #[test]
    fn the_new_file_is_named_after_the_old_one() {
        let folder = PathBuf::from("pictures");
        assert_eq!(
            destination(&folder.join("IMG_0001.HEIC"), Target::Jpeg, None).unwrap(),
            folder.join("IMG_0001.jpg")
        );
        assert_eq!(
            destination(&folder.join("shot.2026.heic"), Target::Png, None).unwrap(),
            folder.join("shot.2026.png")
        );
        assert_eq!(destination(Path::new("a.heic"), Target::WebP, None).unwrap(), PathBuf::from("a.webp"));
        assert_eq!(
            destination(&folder.join("a.heic"), Target::Jpeg, Some(Path::new("out"))).unwrap(),
            Path::new("out").join("a.jpg")
        );
    }

    #[test]
    fn the_summary_says_only_what_happened() {
        assert_eq!(summary(3, 0, 0, 3), "converted 3 of 3");
        assert_eq!(summary(2, 1, 1, 4), "converted 2 of 4, 1 already there, 1 failed");
    }

    /// A taken name is refused by the open itself, and the file in it is not
    /// touched.
    #[test]
    fn a_taken_name_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jpg");
        std::fs::write(&path, b"the picture that was there").unwrap();
        assert!(!write_new(&path, b"another picture").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"the picture that was there");

        let fresh = dir.path().join("b.jpg");
        assert!(write_new(&fresh, b"new").unwrap());
        assert_eq!(std::fs::read(&fresh).unwrap(), b"new");
    }
}
