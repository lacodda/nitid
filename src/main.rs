//! nitid — a fast image viewer for Windows with honest color and HDR.
//!
//! The window and the swapchain belong to this application rather than to a GUI
//! framework: HDR output on Windows is only reachable through `Bt2100Pq` on
//! `Rgb10a2Unorm`, which a framework-managed surface cannot express. See
//! `docs/adr/0001-own-the-swapchain.md`.

// nitid is a console application that hides its console when it opens a
// window. Linking it as a GUI application instead would silence `install`,
// `--version` and every error message: such a process starts with no standard
// handles, and Rust binds them before `main` runs, so they cannot be restored
// afterwards — see `console.rs`.

mod app;
mod console;
mod folder;
mod gpu;
mod image_source;
#[cfg(windows)]
mod install;
mod view;

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
}

fn main() -> ExitCode {
    let command = parse(std::env::args_os().skip(1));

    let result = match command {
        Command::View(path) => {
            // The window is the interface from here on; a console left on
            // screen behind it is noise.
            console::hide_if_ours();
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

    /// A file really can be called `install`; the shell passes a full path in
    /// that case, which is what keeps the two apart.
    #[test]
    fn a_path_ending_in_a_command_name_still_opens() {
        let Command::View(Some(path)) = parse_args(&[r"C:\pictures\install"]) else {
            panic!("a qualified path should open the viewer");
        };
        assert!(path.is_absolute());
    }
}
