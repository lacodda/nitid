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
mod color;
mod config;
mod console;
mod folder;
mod format;
mod gpu;
mod hdr;
mod image_source;
#[cfg(windows)]
mod install;
mod isobmff;
mod loader;
mod sandbox;
mod startup;
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
    pub use crate::image_source::decode_here;
    pub use crate::sandbox::decode as decode_sandboxed;
}

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

/// What the command line asked for.
enum Command {
    /// Show an image, or an empty window when no file was named.
    View(Option<PathBuf>),
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
        Command::View(path) => {
            // The console binary hides the console it was given; the windowed
            // one never had one.
            if !windowed {
                console::hide_if_ours();
            }
            app::run(path)
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
        return Command::View(None);
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
        // since a file may legitimately be named `--version`.
        _ => Command::View(Some(PathBuf::from(first))),
    }
}

fn print_usage() {
    println!(
        "\
nitid {version} — a fast image viewer with honest color

Usage:
  nitid [FILE]
  nitid install
  nitid uninstall

Arguments:
  FILE          image to open; its folder becomes the browsing list

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
        assert!(matches!(parse_args(&[]), Command::View(None)));
    }

    #[test]
    fn a_path_is_opened() {
        let Command::View(Some(path)) = parse_args(&["photo.jpg"]) else {
            panic!("a path should open the viewer");
        };
        assert_eq!(path, PathBuf::from("photo.jpg"));
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
        assert!(matches!(parse_args(&["decode-stdin"]), Command::View(Some(_))));
    }

    /// A file really can be called `install`; the shell passes a qualified
    /// path in that case, which is what keeps the two apart.
    #[test]
    fn a_path_ending_in_a_command_name_still_opens() {
        let qualified = PathBuf::from("pictures").join("install");
        let Command::View(Some(path)) = parse_args(&[&qualified.to_string_lossy()]) else {
            panic!("a qualified path should open the viewer");
        };
        assert_eq!(path, qualified);
    }
}
