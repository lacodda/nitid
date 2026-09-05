//! Measuring the thing the product promises.
//!
//! Startup speed is nitid's whole claim, and a claim that is only ever checked
//! by eye drifts. This records the moment the process began and the moment a
//! picture was first put on screen, and reports the gap when asked.
//!
//! `NITID_STARTUP_REPORT=1` prints the measurement to stderr on the first
//! frame; the release-gate test in `tests/startup.rs` reads that line.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// When this process started, as early as `main` can observe it.
static STARTED: OnceLock<Instant> = OnceLock::new();

/// Set once, when the first frame carrying an image has been presented.
static FIRST_PIXELS: OnceLock<Duration> = OnceLock::new();

/// The environment variable that turns the report on.
const REPORT_VAR: &str = "NITID_STARTUP_REPORT";

/// The environment variable that closes the viewer once it has drawn.
const EXIT_VAR: &str = "NITID_EXIT_AFTER_FIRST_FRAME";

/// Whether the viewer should close as soon as it has shown a picture.
///
/// Only the startup gate sets this: it needs the process to finish so it can
/// read the measurement, and a window waiting for a human would hang it.
///
/// Set to `interface` it waits one frame longer, for the chrome that follows
/// the picture. The gate needs both: that the interface is *not* on the way to
/// the first pixels, and that it arrives immediately after — and a viewer that
/// quit on the first frame could never show the second half.
pub fn exit_after_first_frame() -> bool {
    matches!(exit_when(), Some(ExitWhen::FirstFrame))
}

/// Whether the viewer should close once the interface has been drawn over the
/// picture.
pub fn exit_after_interface() -> bool {
    matches!(exit_when(), Some(ExitWhen::Interface))
}

/// How long the viewer should sit with a picture up before closing itself,
/// when it was asked to.
///
/// `NITID_EXIT_AFTER_FIRST_FRAME=idle:2000` shows the picture, does nothing
/// for two seconds and quits. It is a measuring tool, not a gate: counting
/// the `interface laid out` lines over that window is how the promise that a
/// still picture costs no wakeups gets checked by hand.
///
/// No automatic gate is built on it, and that was tried. The layouts a
/// deliberately looping build produced over the same window measured 740,
/// 184, 178, 16 and 3 across runs — the loop turns out to depend on
/// something outside the process, so a ceiling either passes a broken build
/// or fails a sound one. The measurement is still worth having; the
/// threshold is not.
pub fn idle_for() -> Option<std::time::Duration> {
    match exit_when() {
        Some(ExitWhen::Idle(millis)) => Some(std::time::Duration::from_millis(millis)),
        _ => None,
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
enum ExitWhen {
    FirstFrame,
    Interface,
    /// Stay up this many milliseconds, then quit.
    Idle(u64),
}

fn exit_when() -> Option<ExitWhen> {
    exit_when_from(std::env::var_os(EXIT_VAR)?.to_str()?)
}

/// The parsing on its own, so it can be tested without the environment:
/// setting a variable inside a test races every other test in the process.
fn exit_when_from(text: &str) -> Option<ExitWhen> {
    match text {
        "0" => None,
        "interface" => Some(ExitWhen::Interface),
        text if text.starts_with("idle:") => {
            // A malformed window still gives one: a run that quit at once
            // would measure nothing.
            Some(ExitWhen::Idle(text.trim_start_matches("idle:").parse().unwrap_or(2000)))
        }
        _ => Some(ExitWhen::FirstFrame),
    }
}

/// The prefix of the reported line, so a test can find it unambiguously.
pub const REPORT_PREFIX: &str = "nitid: first pixels in ";

/// Start the clock. Called first thing in `main`.
pub fn begin() {
    let _ = STARTED.set(Instant::now());
}

/// Note how long a startup step took, when the report is switched on.
///
/// Startup is a sequence of costs — the GPU device, the swapchain, the first
/// decode — and knowing the total without the breakdown says nothing about
/// what to fix.
pub fn milestone(what: &str) {
    let (Some(started), true) = (STARTED.get(), reporting()) else {
        return;
    };
    eprintln!("nitid: {what} at {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
}

fn reporting() -> bool {
    std::env::var_os(REPORT_VAR).is_some_and(|value| value != "0")
}

/// The prefix of the line naming the output signal, so a test can find it.
pub const SURFACE_PREFIX: &str = "nitid: surface ";

/// Report how the swapchain is configured, when the report is switched on.
///
/// Colour is the product's second promise, and high dynamic range is the part
/// of it that cannot be confirmed by looking at a screenshot — a screenshot is
/// standard range whatever the surface was. Stating the configuration turns
/// "is HDR on?" into something a person or a test can read rather than judge.
pub fn surface(format: &str, color_space: &str, headroom: Option<f32>) {
    if !reporting() {
        return;
    }
    match headroom {
        Some(headroom) => eprintln!("{SURFACE_PREFIX}{format} {color_space}, display headroom {headroom:.2}x"),
        None => eprintln!("{SURFACE_PREFIX}{format} {color_space}, display headroom unknown"),
    }
}

/// Record that a frame with an image in it has reached the screen.
///
/// Only the first call counts: later frames are the viewer running, not the
/// viewer starting.
pub fn first_pixels() {
    let Some(started) = STARTED.get() else {
        return;
    };
    if FIRST_PIXELS.set(started.elapsed()).is_err() {
        return;
    }

    if reporting() {
        // stderr rather than stdout: a measurement is diagnostics, and stdout
        // belongs to whatever the command was actually asked to print.
        eprintln!(
            "{REPORT_PREFIX}{:.1} ms",
            FIRST_PIXELS.get().copied().unwrap_or_default().as_secs_f64() * 1000.0
        );
    }
}

/// The prefix of the line saying the interface reached the screen.
pub const INTERFACE_PREFIX: &str = "nitid: interface on screen at ";

/// Record that a frame carrying the chrome has reached the screen.
///
/// Reported unconditionally rather than under the report flag, because the
/// gate that reads it is asking about order, not about speed: it needs to see
/// that this happened *after* the first pixels, on a run where the report may
/// or may not be on.
pub fn interface_drawn() {
    let Some(started) = STARTED.get() else {
        return;
    };
    eprintln!("{INTERFACE_PREFIX}{:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
}

/// How long the first picture took to appear, once it has.
#[cfg(test)]
pub fn elapsed() -> Option<Duration> {
    FIRST_PIXELS.get().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The idle mode's duration is read from the value, and a malformed one
    /// still gives a window rather than quitting at once — a gate that exited
    /// immediately would measure nothing and pass.
    #[test]
    fn the_idle_mode_carries_how_long_to_wait() {
        assert_eq!(
            exit_when_from("idle:500"),
            Some(ExitWhen::Idle(500)),
            "the idle window was not read from the value"
        );
        assert_eq!(exit_when_from("idle:nonsense"), Some(ExitWhen::Idle(2000)));
        assert_eq!(exit_when_from("interface"), Some(ExitWhen::Interface));
        assert_eq!(exit_when_from("1"), Some(ExitWhen::FirstFrame));
        assert_eq!(exit_when_from("0"), None);
    }

    #[test]
    fn recording_without_a_started_clock_is_harmless() {
        // The unit-test binary never calls `begin`, so this exercises the
        // path where a frame is drawn by something that did not start a clock.
        first_pixels();
        assert!(elapsed().is_none());
    }

    #[test]
    fn the_surface_prefix_ends_where_its_description_begins() {
        assert!(SURFACE_PREFIX.ends_with(' '));
    }

    #[test]
    fn the_report_prefix_ends_where_a_number_begins() {
        // The gate test parses the line by stripping this prefix; a trailing
        // space that drifted away would break that silently.
        assert!(REPORT_PREFIX.ends_with(' '));
    }
}
