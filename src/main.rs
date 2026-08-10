//! nitid — a fast image viewer for Windows with honest color and HDR.
//!
//! The window and the swapchain belong to this application rather than to a GUI
//! framework: HDR output on Windows is only reachable through `Bt2100Pq` on
//! `Rgb10a2Unorm`, which a framework-managed surface cannot express. See
//! `docs/adr/0001-own-the-swapchain.md`.

// A viewer launched from the shell must not drag a console window along with
// it; a debug build keeps one so `eprintln!` diagnostics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod folder;
mod gpu;
mod image_source;
mod view;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next().map(PathBuf::from);

    if let Some(path) = first.as_ref().and_then(|path| path.to_str())
        && matches!(path, "-h" | "--help" | "/?")
    {
        print_usage();
        return ExitCode::SUCCESS;
    }

    if let Some(path) = first.as_ref().and_then(|path| path.to_str())
        && matches!(path, "-V" | "--version")
    {
        println!("nitid {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    match app::run(first) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nitid: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "\
nitid {version} — a fast image viewer with honest color

Usage:
  nitid [FILE]

Arguments:
  FILE          image to open; its folder becomes the browsing list

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
