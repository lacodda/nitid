//! The console-subsystem binary: `nitid`.
//!
//! This is the one to run from a terminal. It can print, so `install`,
//! `--version` and error messages work. When it opens a window it hides the
//! console it was given, unless that console belongs to a terminal it joined.
//!
//! The shell association uses `nitidw.exe` instead, which never has a console
//! to flash. See `docs/adr/0004-two-binaries-console-and-windowed.md`.

use std::process::ExitCode;

fn main() -> ExitCode {
    nitid::run(false)
}
