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
mod clipboard;
mod color;
mod config;
mod console;
mod drag;
mod eyedropper;
mod folder;
mod format;
mod gpu;
mod hdr;
mod histogram;
mod icon;
mod image_source;
#[cfg(windows)]
mod install;
mod interface;
mod isobmff;
mod loader;
mod metadata;
mod sandbox;
// The whole mechanism is Windows shell behaviour: the pipe, the election, and
// the multi-select it answers. There is nothing here for another platform to
// compile.
#[cfg(windows)]
mod single;
mod startup;
mod tiles;
mod vector;
mod view;

/// The pieces the integration tests reach into.
///
/// Exposed for `tests/`, which drives the colour path end to end against files
/// it writes itself. Nothing outside this crate's own tests should depend on
/// it: the viewer is a program, not a library anyone else builds on.
#[doc(hidden)]
pub mod testing {
    pub use crate::color::{ColorTransform, profile_from};
    pub use crate::format::Format;
    pub use crate::image_source::{Depth, decode_here};
    pub use crate::metadata::read as read_metadata;
    pub use crate::sandbox::decode as decode_sandboxed;

    /// Every key the key sheet advertises, for the test that holds the README
    /// to it. The two lists are written for different readers and drifted
    /// apart silently until v0.23.1 tied them together.
    pub fn keys() -> &'static [(&'static str, &'static str)] {
        crate::interface::KEYS
    }

    /// Every extension the viewer opens and the installer registers.
    pub fn extensions() -> Vec<&'static str> {
        crate::image_source::supported_extensions()
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
    if windowed && !matches!(command, Command::View(_) | Command::Decode) {
        return delegate_to_console_binary();
    }

    let result = match command {
        Command::View(paths) => {
            // The console binary hides the console it was given; the windowed
            // one never had one.
            if !windowed {
                console::hide_if_ours();
            }
            view(paths)
        }
        Command::Help => {
            print_usage();
            Ok(())
        }
        Command::Version => {
            println!("nitid {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Decode => sandbox::run_as_decoder(),
        #[cfg(windows)]
        Command::Install => install::install().map(|_| ()),
        #[cfg(windows)]
        Command::Uninstall => install::uninstall(),
        #[cfg(not(windows))]
        Command::Install | Command::Uninstall => Err(anyhow::anyhow!("installing is a Windows-only command")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
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
fn view(paths: Vec<PathBuf>) -> anyhow::Result<()> {
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
            Ok(Some(listener)) => return app::run_owning(paths, listener),
            // Somebody is. Hand over and go.
            Ok(None) => {
                if !paths.is_empty() && single::channel::send(&name, &paths, single::HANDOVER_PATIENCE).is_ok() {
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

    app::run(paths)
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
        // Leading dashes, so it cannot be mistaken for a file: every other
        // unrecognised argument is treated as a path.
        Some(sandbox::DECODE_ARGUMENT) => Command::Decode,
        // Anything else is a path — including one that looks like a flag,
        // since a file may legitimately be named `--version`. Every remaining
        // argument is a path too: a command line naming several files opens
        // them as one list rather than showing only the first.
        _ => Command::View(std::iter::once(first).chain(args).map(PathBuf::from).collect()),
    }
}

fn print_usage() {
    println!(
        "\
nitid {version} — a fast image viewer with honest color

Usage:
  nitid [FILE...]
  nitid install
  nitid uninstall

Arguments:
  FILE...       images to open. One file browses its folder; several browse
                themselves. A viewer already open takes them rather than a
                second window starting

Commands:
  install       copy nitid to %LOCALAPPDATA%\\Programs and register its file
                types, so it is offered in \"Open with\"
  uninstall     undo the above

Options:
  -h, --help    print this message
  -V, --version print the version

Keys:
  Left/Right    previous / next image in the folder
  Home/End      first / last image
  Space         pause / resume an animation; next image on a still
  Wheel         zoom around the cursor
  Drag          pan
  Middle click  toggle fit and 100%
  0 / 1         fit to window / actual size
  F11           fullscreen
  Esc           quit",
        version = env!("CARGO_PKG_VERSION")
    );
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
        assert!(matches!(parse_args(&["--help"]), Command::Help));
        assert!(matches!(parse_args(&["-V"]), Command::Version));
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
