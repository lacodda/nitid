//! nitid — a fast image viewer for Windows with honest color and HDR.
//!
//! The window and the swapchain belong to this application rather than to a GUI
//! framework: HDR output needs a surface format and colour space of its own,
//! which a framework-managed surface picks for itself. See
//! `docs/adr/0001-own-the-swapchain.md` and
//! `docs/adr/0013-hdr-output-goes-through-scrgb.md`.
//!
//! This crate is the whole viewer; the two binaries over it differ only in
//! which Windows subsystem they are linked for. See
//! `docs/adr/0004-two-binaries-console-and-windowed.md`.

mod animation;
mod app;
mod avif;
mod batch;
// Only Windows has the dialog; the list of what it filters on is shared.
#[cfg_attr(not(windows), allow(dead_code))]
mod browse;
mod clipboard;
mod color;
#[cfg(windows)]
mod com;
mod compare;
mod config;
mod console;
mod convert;
mod crop;
mod cull;
mod drag;
mod editor;
mod export;
mod eyedropper;
mod files;
mod fit;
mod folder;
mod format;
mod gpu;
mod hdr;
mod histogram;
// The listener is Windows' alone; the combination itself is read and written
// everywhere, because the settings that hold it are.
#[cfg_attr(not(windows), allow(dead_code))]
mod hotkey;
mod icon;
mod image_source;
mod info;
#[cfg(windows)]
mod install;
mod interface;
mod isobmff;
mod jpeg_lossless;
mod json;
mod loader;
mod metadata;
mod minimap;
mod pattern;
// The page is laid out and tested everywhere, but only Windows has a print
// system to hand it to: elsewhere `print::show` says so, and the layout goes
// unused outside the tests. The arrangement `sandbox::protocol` uses.
#[cfg_attr(not(windows), allow(dead_code))]
mod print;
mod recent;
// What stays behind once the windows close: Explorer's space bar, and the
// icon to tell it to go. All of it is Windows shell behaviour.
#[cfg(windows)]
mod resident;
mod rotate;
mod sandbox;
mod scrub;
// The whole mechanism is Windows shell behaviour: the pipe, the election, and
// the multi-select it answers. There is nothing here for another platform to
// compile.
#[cfg(windows)]
mod single;
mod slideshow;
mod startup;
mod theme;
mod tiles;
mod vector;
mod view;
mod wallpaper;
mod watch;

/// The pieces the integration tests reach into.
///
/// Exposed for `tests/`, which drives the colour path end to end against files
/// it writes itself. Nothing outside this crate's own tests should depend on
/// it: the viewer is a program, not a library anyone else builds on.
#[doc(hidden)]
pub mod testing {
    pub use crate::color::{ColorTransform, profile_from};
    pub use crate::format::Format;
    pub use crate::image_source::Orientation;
    pub use crate::image_source::{Depth, decode_here};
    pub use crate::metadata::read as read_metadata;
    pub use crate::rotate::save_orientation;
    pub use crate::sandbox::decode as decode_sandboxed;

    /// Write a loaded image out, the way the save box does.
    ///
    /// Here so a run over real files can exercise the export itself rather
    /// than a copy of it: the save box is behind a window, and a gate that
    /// re-implemented the request would be testing its own re-implementation.
    pub fn export_for_test(loaded: &crate::image_source::LoadedImage, extension: &str, bake: bool, quality: u8) -> anyhow::Result<Vec<u8>> {
        let target = crate::export::Target::from_extension(extension).ok_or_else(|| anyhow::anyhow!("no target named {extension}"))?;
        crate::export::encode(&crate::export::Request {
            image: &loaded.image,
            profile: loaded.profile.as_ref(),
            target,
            colour: if bake {
                crate::export::Colour::BakeToSrgb
            } else {
                crate::export::Colour::KeepProfile
            },
            quality,
            caveat: loaded.caveat.as_deref(),
            orientation: loaded.orientation,
            exif: None,
        })
    }

    /// The lossless crop, for a gate that runs it over real files.
    ///
    /// Exposed rather than re-implemented in the test for the reason the
    /// export is: a gate that rebuilt the path would be checking its own
    /// rebuild.
    pub use crate::jpeg_lossless::{Grid, Rect, Refusal, crop as crop_losslessly, grid_of, snap};

    /// Baking a turn into a JPEG's coefficients, and taking a file's metadata
    /// out of it. Exposed for the same reason the crop is: a gate that runs
    /// over real files must run the viewer's own path, not a copy.
    pub use crate::jpeg_lossless::{Turn, turn as turn_losslessly};
    pub use crate::scrub::{Found, Keep, scrub, supported as can_scrub, survey};

    /// Cut a rectangle out of a decoded picture, the way the re-encoding
    /// fallback does.
    pub use crate::export::cut;

    /// A sheet and its printable area, in the print system's units.
    pub use crate::print::{Area as PrintArea, Page as PrintPage};

    /// Print a file into an XPS document rather than to a printer: the page
    /// the print dialog hands a printer, drawn by the same code from the same
    /// job, for a gate that runs where there is no printer at all.
    #[cfg(windows)]
    pub fn print_to_xps(source: &std::path::Path, page: crate::print::Page, fill: bool, out: &std::path::Path) -> anyhow::Result<()> {
        let bytes = std::fs::read(source)?;
        let loaded = crate::image_source::decode(&bytes)?;
        let picture = match &loaded.vector {
            Some(vector) => crate::print::Picture::Vector(vector.clone()),
            None => crate::print::Picture::Raster {
                image: loaded.image.clone(),
                profile: loaded.profile.clone().map(Box::new),
            },
        };
        let job = crate::print::Job {
            title: source.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
            picture,
            orientation: loaded.orientation,
        };
        let fit = if fill { crate::fit::Fit::Fill } else { crate::fit::Fit::Fit };
        crate::print::write_xps(&job, page, fit, out)
    }

    /// Make the desktop's picture from a file, for the displays named — the
    /// very function `W`, `Shift+W` and the schedule hand to Windows, so a
    /// gate over real files of every format runs the path itself and never
    /// touches the desktop.
    pub fn prepare_wallpaper(path: &std::path::Path, screens: &[(u32, u32)], fill: bool) -> anyhow::Result<Vec<u8>> {
        crate::wallpaper::prepare(path, screens, if fill { crate::fit::Fit::Fill } else { crate::fit::Fit::Fit })
    }

    /// Marking a picture, for the gate that asks Windows whether it can see
    /// the mark. Named after what they mean rather than exposing the enum,
    /// so the gate reads as the promise it holds: a kept picture shows a star
    /// in Explorer, a cleared one does not, and a rejected one is not mistaken
    /// for a rating.
    pub fn mark_kept(path: &std::path::Path) -> anyhow::Result<()> {
        crate::cull::write(path, crate::cull::Mark::Keep)
    }

    pub fn mark_rejected(path: &std::path::Path) -> anyhow::Result<()> {
        crate::cull::write(path, crate::cull::Mark::Reject)
    }

    pub fn mark_cleared(path: &std::path::Path) -> anyhow::Result<()> {
        crate::cull::write(path, crate::cull::Mark::Unmarked)
    }

    pub fn is_rejected(path: &std::path::Path) -> bool {
        crate::cull::read(path) == crate::cull::Mark::Reject
    }

    /// Every key the key sheet advertises, for the test that holds the README
    /// to it. The two lists are written for different readers and drifted
    /// apart silently until v0.23.1 tied them together.
    pub fn keys() -> &'static [(&'static str, &'static str)] {
        crate::interface::KEYS
    }

    /// What `--help` prints, for the test that holds the command-line page
    /// to it.
    pub fn usage() -> String {
        crate::usage()
    }

    /// Every extension the viewer opens and the installer registers.
    pub fn extensions() -> Vec<&'static str> {
        crate::image_source::supported_extensions()
    }

    /// Whether a resident is running under the instance `id` names, and
    /// telling it to go: for the test that starts two and expects one.
    #[cfg(windows)]
    pub fn resident_running(id: &str) -> bool {
        // SAFETY: as for `instance_is_listening` below.
        unsafe { std::env::set_var("NITID_INSTANCE_ID", id) };
        crate::resident::running()
    }

    #[cfg(windows)]
    pub fn stop_resident(id: &str) -> anyhow::Result<bool> {
        // SAFETY: as for `instance_is_listening` below.
        unsafe { std::env::set_var("NITID_INSTANCE_ID", id) };
        crate::resident::stop()
    }

    /// Whether a viewer is listening on the channel `id` names.
    ///
    /// The single-instance tests have to wait for the first viewer to be
    /// ready before a second one can hand anything over, and asking the
    /// channel is the only way that does not involve starting a viewer with
    /// no file — which would open a window that never exits.
    #[cfg(windows)]
    pub fn instance_is_listening(id: &str) -> bool {
        // SAFETY: the tests are the only caller, and they set this to a value
        // of their own before starting any viewer.
        unsafe { std::env::set_var("NITID_INSTANCE_ID", id) };
        crate::single::channel::is_listening(&crate::single::pipe_name())
    }
}

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// What the command line asked for.
enum Command {
    /// Show the images named, or an empty window when none were.
    ///
    /// A list rather than one path because the shell can hand over several:
    /// selecting five files and pressing Enter starts five processes, whose
    /// paths are gathered into one window. See `single`.
    View(Vec<PathBuf>),
    /// Show the images named as a look from Explorer's space bar, which the
    /// space bar ends. Not typed: the resident starts a window this way when
    /// none is open (see `resident`).
    Peek(Vec<PathBuf>),
    /// Be the resident: stay behind, answer Explorer's space bar, keep an
    /// icon in the notification area. Not typed either: the sign-in entry
    /// `nitid install` writes starts it.
    Resident,
    Install,
    Uninstall,
    Help,
    Version,
    /// Be the sandboxed decoder: read a file on stdin, write pixels on stdout.
    ///
    /// Not a command anyone types. The viewer launches its own executable this
    /// way so a C decoder runs somewhere it can do no harm — see
    /// `docs/adr/0002-sandbox-c-decoders.md`.
    Decode,
    /// The desktop's picture: from a folder on a schedule, the next one now,
    /// or no longer changing. See `wallpaper`.
    Wallpaper(wallpaper::Asked),
    /// What pictures say about themselves, printed. See `info`.
    Info(info::Asked),
    /// Pictures written again in another format. See `convert`.
    Convert(convert::Asked),
    /// Something the command line does not take, said before anything is
    /// done rather than mistaken for a file to open.
    Mistake(String),
}

/// Run the viewer, whichever binary was launched.
///
/// `windowed` says which one: the GUI-subsystem `nitidw.exe` has no console to
/// print to, so a command that only prints has nothing to do there.
pub fn run(windowed: bool) -> ExitCode {
    // Before anything else: the measurement is of startup, so it starts here.
    startup::begin();

    let command = parse(std::env::args_os().skip(1));

    // `nitidw.exe install` would do its work and report it to nobody, so the
    // printing commands are handed to the binary that has somewhere to print.
    // This is a safety net rather than a path anyone takes: the windowed
    // binary exists to be launched by the shell with a file.
    // `wallpaper next` is the exception that proves it: the scheduled task
    // runs the windowed binary precisely so that no console window flashes up
    // on the desktop every half hour, and it prints nothing when it succeeds.
    // The resident and a look from Explorer are the windowed binary's by the
    // same right: nothing they do is printed, and a console would flash.
    if windowed
        && !matches!(
            command,
            Command::View(_) | Command::Peek(_) | Command::Resident | Command::Decode | Command::Wallpaper(wallpaper::Asked::Next)
        )
    {
        return delegate_to_console_binary();
    }

    let result = match command {
        Command::View(paths) => {
            // The console binary hides the console it was given; the windowed
            // one never had one.
            if !windowed {
                console::hide_if_ours();
            }
            view(paths, false)
        }
        Command::Peek(paths) => {
            if !windowed {
                console::hide_if_ours();
            }
            view(paths, true)
        }
        #[cfg(windows)]
        Command::Resident => resident::run(),
        #[cfg(not(windows))]
        Command::Resident => Err(anyhow::anyhow!("the resident is a Windows-only part of nitid")),
        Command::Help => {
            println!("{}", usage());
            Ok(())
        }
        Command::Version => {
            println!("nitid {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Decode => sandbox::run_as_decoder(),
        Command::Wallpaper(asked) => wallpaper::command(asked),
        Command::Info(asked) => info::command(asked),
        Command::Convert(asked) => convert::command(asked),
        Command::Mistake(mistake) => Err(anyhow::anyhow!("{mistake} (nitid --help says what it takes)")),
        #[cfg(windows)]
        Command::Install => install::install().map(|_| ()),
        #[cfg(windows)]
        Command::Uninstall => install::uninstall(),
        #[cfg(not(windows))]
        Command::Install | Command::Uninstall => Err(anyhow::anyhow!("installing is a Windows-only command")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        // Said already, file by file, where it happened.
        Err(error) if error.is::<batch::Reported>() => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("nitid: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Show `paths`, in this process or in the window that is already open.
///
/// Starting a viewer costs a window and a graphics device — measured at 190 to
/// 340 milliseconds of a 320 to 560 millisecond cold start. A viewer that is
/// already running has both, so when one is found this process hands its files
/// over and exits, and the picture appears in the time it takes to decode it.
///
/// Every way of failing to hand over ends in opening a window here: no window
/// to talk to, a pipe that cannot be created, a message that will not send.
/// Failing to share is never a reason to refuse to show a picture.
///
/// `peek` is a look from Explorer's space bar, carried to whichever window
/// shows it (see `resident`).
fn view(paths: Vec<PathBuf>, peek: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    if single::enabled() {
        // Relative paths mean different things in different processes: the
        // shell gives each launch its own working directory. Resolved here,
        // where the original one still applies.
        let paths = single::absolute(&paths);
        let name = single::pipe_name();

        match single::channel::claim(&name) {
            // Nobody else is up: this process is the window. It keeps the
            // listener and hands it to the event loop.
            Ok(Some(listener)) => return app::run_owning(paths, peek, listener),
            // Somebody is. Hand over and go.
            Ok(None) => {
                let kind = if peek { single::Kind::Peek } else { single::Kind::Open };
                if !paths.is_empty() && single::channel::send(&name, &paths, kind, single::HANDOVER_PATIENCE).is_ok() {
                    return Ok(());
                }
                // Either there was nothing to hand over — a bare launch, which
                // should open its own window rather than silently do nothing —
                // or the window went away between the claim and the message.
            }
            // The pipe could not be made for some other reason. Say so where
            // there is somewhere to say it, and open a window anyway.
            Err(error) => eprintln!("nitid: {error:#}"),
        }
    }

    app::run(paths, peek)
}

/// Re-run this command as `nitid.exe`, which has a console.
///
/// The two binaries sit in the same directory by construction: they are built
/// together and installed together.
fn delegate_to_console_binary() -> ExitCode {
    let Ok(mut exe) = std::env::current_exe() else {
        return ExitCode::FAILURE;
    };
    exe.set_file_name(if cfg!(windows) { "nitid.exe" } else { "nitid" });

    let status = std::process::Command::new(exe).args(std::env::args_os().skip(1)).status();

    match status {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

/// How the resident is started: by the sign-in entry and by `nitid install`.
/// Dashed like the decoder's, so a file of that name is never taken for it.
const RESIDENT_ARGUMENT: &str = "--resident";

/// How the resident starts a window for a look from Explorer when none is
/// open. The paths follow it.
const PEEK_ARGUMENT: &str = "--peek";

fn parse(args: impl Iterator<Item = OsString>) -> Command {
    let mut args = args.peekable();
    let Some(first) = args.next() else {
        return Command::View(Vec::new());
    };

    match first.to_str() {
        Some("-h" | "--help" | "/?") => Command::Help,
        Some("-V" | "--version") => Command::Version,
        Some("install") => Command::Install,
        Some("uninstall") => Command::Uninstall,
        Some("wallpaper") => {
            let rest: Vec<String> = args.map(|arg| arg.to_string_lossy().into_owned()).collect();
            match wallpaper::Asked::parse(&rest) {
                Ok(asked) => Command::Wallpaper(asked),
                Err(error) => Command::Mistake(format!("{error:#}")),
            }
        }
        // The paths stay as the system gave them: a name that is not valid
        // Unicode is still a file somebody can ask about.
        Some("info") => match info::Asked::parse(args) {
            Ok(asked) => Command::Info(asked),
            Err(error) => Command::Mistake(format!("{error:#}")),
        },
        Some("convert") => match convert::Asked::parse(args) {
            Ok(asked) => Command::Convert(asked),
            Err(error) => Command::Mistake(format!("{error:#}")),
        },
        // Leading dashes, so it cannot be mistaken for a file: every other
        // unrecognised argument is treated as a path.
        Some(sandbox::DECODE_ARGUMENT) => Command::Decode,
        Some(RESIDENT_ARGUMENT) => Command::Resident,
        Some(PEEK_ARGUMENT) => Command::Peek(args.map(PathBuf::from).collect()),
        // Anything else is a path — including one that looks like a flag,
        // since a file may legitimately be named `--version`. Every remaining
        // argument is a path too: a command line naming several files opens
        // them as one list rather than showing only the first.
        _ => Command::View(std::iter::once(first).chain(args).map(PathBuf::from).collect()),
    }
}

/// What `--help` prints: every command and option, and the keys.
fn usage() -> String {
    format!(
        "\
nitid {version} — a fast image viewer with honest color

Usage:
  nitid [FILE...]
  nitid install
  nitid uninstall
  nitid wallpaper [FOLDER] [--every 30m] [--only PATTERN] [--fit fill|fit]
  nitid wallpaper next | off
  nitid info FILE... [--json]
  nitid convert FILE... --to jpeg|png|webp [--quality 1-100] [--srgb] [--out FOLDER]

Arguments:
  FILE...       images to open. One file browses its folder; several browse
                themselves; a folder browses its pictures from the first. A
                viewer already open takes them rather than a second window
                starting. With none, the folders opened lately are shown,
                each at the picture last looked at there

Commands:
  install       copy nitid to %LOCALAPPDATA%\\Programs and register its file
                types, so it is offered in \"Open with\"
  uninstall     undo the above
  wallpaper     change the desktop's picture from FOLDER on a schedule, every
                30 minutes unless --every says otherwise (5m, 2h, 1d). --only
                takes names matching a pattern such as *.png; --fit fill
                covers the screen, --fit fit shows the whole picture. With
                no FOLDER it changes how the running schedule works, or says
                what it is doing; next puts the next picture up now, off
                stops it
  info          what each picture says about itself: size, format, colour
                profile, camera, place. --json prints the same for a script
  convert       write each picture again as JPEG, PNG or WebP, beside it or
                in --out FOLDER, keeping its colour profile (--srgb converts
                the colour instead), its EXIF and its mark. Nothing is
                replaced: a name already taken is left alone

  FILE can be a picture, a folder (its pictures) or a pattern such as
  *.heic, expanded by nitid itself because Windows shells do not

Options:
  -h, --help    print this message
  -V, --version print the version

Keys:
  Left/Right    previous / next image in the folder
  Home/End      first / last image
  Space         pause / resume an animation; next image on a still; in a
                look from Explorer, put it away
  Wheel         zoom around the cursor
  Drag          pan
  Middle click  toggle fit and 100%
  0 / 1         fit to window / actual size
  O             the folders opened lately; Ctrl+O opens another picture
  F11           fullscreen
  Esc           quit",
        version = env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Command {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_opens_an_empty_window() {
        assert!(matches!(parse_args(&[]), Command::View(paths) if paths.is_empty()));
    }

    #[test]
    fn a_path_is_opened() {
        let Command::View(paths) = parse_args(&["photo.jpg"]) else {
            panic!("a path should open the viewer");
        };
        assert_eq!(paths, vec![PathBuf::from("photo.jpg")]);
    }

    /// The shell hands one file per launch, but a command line — and a
    /// hand-over from another instance — can carry several.
    #[test]
    fn several_paths_are_all_opened() {
        let Command::View(paths) = parse_args(&["one.jpg", "two.png", "three.webp"]) else {
            panic!("several paths should open the viewer");
        };
        assert_eq!(paths, vec![PathBuf::from("one.jpg"), PathBuf::from("two.png"), PathBuf::from("three.webp")]);
    }

    #[test]
    fn the_commands_are_recognised() {
        assert!(matches!(parse_args(&["install"]), Command::Install));
        assert!(matches!(parse_args(&["uninstall"]), Command::Uninstall));
        assert!(matches!(parse_args(&["wallpaper"]), Command::Wallpaper(wallpaper::Asked::Status)));
        assert!(matches!(parse_args(&["wallpaper", "next"]), Command::Wallpaper(wallpaper::Asked::Next)));
        assert!(matches!(parse_args(&["wallpaper", "--every", "soon"]), Command::Mistake(_)));
        assert!(matches!(parse_args(&["info", "a.jpg"]), Command::Info(_)));
        assert!(matches!(parse_args(&["info"]), Command::Mistake(_)));
        assert!(matches!(parse_args(&["convert", "*.heic", "--to", "jpeg"]), Command::Convert(_)));
        assert!(matches!(parse_args(&["convert", "*.heic"]), Command::Mistake(_)));
        assert!(matches!(parse_args(&["--help"]), Command::Help));
        assert!(matches!(parse_args(&["-V"]), Command::Version));
    }

    /// The resident's own arguments are recognised, and a look carries its
    /// paths like an open does.
    #[test]
    fn the_residents_arguments_are_recognised() {
        assert!(matches!(parse_args(&[RESIDENT_ARGUMENT]), Command::Resident));
        let Command::Peek(paths) = parse_args(&[PEEK_ARGUMENT, "one.jpg", "two.png"]) else {
            panic!("a look should open the viewer");
        };
        assert_eq!(paths, vec![PathBuf::from("one.jpg"), PathBuf::from("two.png")]);
        // Bare words of the same name are files, as every other bare word is.
        assert!(matches!(parse_args(&["resident"]), Command::View(paths) if paths.len() == 1));
        assert!(matches!(parse_args(&["peek"]), Command::View(paths) if paths.len() == 1));
    }

    /// The decoder argument is spelled with dashes precisely so a file cannot
    /// take its place: an unrecognised bare word is opened as a path.
    #[test]
    fn the_decoder_argument_is_not_a_path() {
        assert!(matches!(parse_args(&[sandbox::DECODE_ARGUMENT]), Command::Decode));
        assert!(matches!(parse_args(&["decode-stdin"]), Command::View(paths) if paths.len() == 1));
    }

    /// A file really can be called `install`; the shell passes a qualified
    /// path in that case, which is what keeps the two apart.
    #[test]
    fn a_path_ending_in_a_command_name_still_opens() {
        let qualified = PathBuf::from("pictures").join("install");
        let Command::View(paths) = parse_args(&[&qualified.to_string_lossy()]) else {
            panic!("a qualified path should open the viewer");
        };
        assert_eq!(paths, vec![qualified]);
    }
}
