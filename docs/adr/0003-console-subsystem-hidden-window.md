# 0003 — nitid links as a console application and hides the console

Date: 2026-08-10
Status: accepted

## Context

nitid is a viewer, so the obvious choice is to link it as a Windows GUI
subsystem application (`#![windows_subsystem = "windows"]`). That is what keeps
a console window from flashing up when the shell launches it by double-click.

It also has commands that print: `install`, `uninstall`, `--version`, `--help`,
and every error message the viewer can produce before a window exists.

A GUI subsystem process starts with no standard handles at all. Rust binds
`println!` and `eprintln!` to those handles before `main` runs, so by the time
any code could call `AttachConsole(ATTACH_PARENT_PROCESS)`, the streams are
already bound to nothing. Attaching afterwards does not rebind them.

This was measured rather than assumed: with the GUI subsystem, `nitid install`
produced no output when run from PowerShell **or** from cmd, and — the
telling case — none even when redirected to a file, which rules out the console
window as the missing piece. `SetStdHandle` onto `CONOUT$` after attaching did
not fix it either, because the C runtime's own file descriptors had already
been established.

## Decision

nitid links as a console subsystem application. On the viewing path only, it
hides the console window it was given, and only when that window belongs to
this process alone.

Ownership is checked with `GetConsoleProcessList`: a console created for a
double-click lists exactly one process. A console shared with a terminal lists
more, and hiding it would take the user's shell window down with it.

## Consequences

Positive:

- Every command prints, from every launcher, redirected or not.
- Errors reaching the user are possible before a window exists — a viewer that
  fails to create a device can say so.
- Double-clicking an image shows one window, the viewer's, as before.

Negative:

- A console is allocated and then hidden on the shell path. It is cheap, but it
  is not free, and it can appear for a few frames on a slow machine. If that
  ever shows up in the startup measurements of v0.2.0, the answer is a second
  binary (`nitidw.exe`) for the shell association, not reverting this.
- The check is Windows-specific, sitting behind `#[cfg(windows)]` like the rest
  of the shell integration.

Rejected alternatives:

- **GUI subsystem plus `AttachConsole`** — the obvious approach, and the one
  tried first. It does not work for the reason above.
- **A separate console binary for the commands** — two executables to install,
  two to keep in step, and `nitid --version` would have to be the wrong one
  half the time.
- **Silent commands** — an installer that reports nothing gives the user no way
  to tell success from failure.
