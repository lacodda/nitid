//! The windowed binary: `nitidw`.
//!
//! Linked for the Windows GUI subsystem, so opening an image from Explorer
//! creates no console at all — not even one that is hidden a few milliseconds
//! later, which is visible as a flash on a cold start.
//!
//! The cost is that this binary cannot print: a GUI-subsystem process starts
//! with no standard handles, and Rust binds `println!` to them before `main`
//! runs. That is why the commands that report live in `nitid.exe`, and why
//! this one hands them over when it is given one.
//!
//! See `docs/adr/0004-two-binaries-console-and-windowed.md`.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    nitid::run(true)
}
