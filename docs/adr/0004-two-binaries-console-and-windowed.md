# 0004 — two binaries: `nitid` prints, `nitidw` opens files

Date: 2026-08-10
Status: accepted
Supersedes: part of [0003](0003-console-subsystem-hidden-window.md)

## Context

ADR 0003 linked nitid as a console application and hid the console window when
the viewer opened a window. That kept `install`, `--version` and error messages
working, which a GUI-subsystem binary cannot do: such a process starts with no
standard handles, and Rust binds `println!` to them before `main` runs.

It also predicted the cost, and the prediction came true in daily use: the
console is allocated by Windows before `main` gets control, so on a cold start
it is visible for a moment before being hidden. Opening an image from Explorer
flashes a black rectangle. ADR 0003 named the answer in advance — a separate
windowed binary for the shell association — and left it until the cost showed
up. It has.

## Decision

The crate becomes a library with two thin binaries over it:

- **`nitid.exe`** — console subsystem. Run from a terminal. Prints, so
  `install`, `uninstall`, `--version`, `--help` and error messages work. It
  still hides a console it owns when it opens a window, which covers the case
  of launching it by hand from Explorer.
- **`nitidw.exe`** — GUI subsystem. No console is ever created, so nothing
  flashes. This is the binary registered for file associations.

Both are installed together; `install` refuses to proceed if either is missing,
because an association pointing at an absent executable is worse than no
association. Given a printing command, `nitidw.exe` re-runs it as `nitid.exe`
rather than doing the work silently.

## Consequences

Positive:

- Opening an image from Explorer creates no console at all — the flash is gone
  by construction, not by racing to hide a window.
- The commands that report still report, from the binary a terminal runs.
- The library split makes the viewer testable as a library, which the startup
  gate already benefits from.

Negative:

- Two executables to install, package and keep in step. The install command
  treats them as a pair to make that a single failure rather than a silent
  half-install.
- Users who type `nitidw` and expect output get a delegated run instead. The
  naming convention (`w` for windowed) follows `python`/`pythonw`, which is
  where the expectation comes from in the first place.

Rejected alternatives:

- **Keep one console binary and hide faster** — the console exists before any
  of our code runs. There is no "faster" available.
- **One GUI binary, report through message boxes** — an installer whose output
  cannot be piped, logged, or read by CI. `install` is a command-line command.
- **One GUI binary that re-executes itself with a console** — doubles process
  startup on the path where startup is the product's whole claim.
