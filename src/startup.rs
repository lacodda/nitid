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

/// Whether the viewer should close as soon as it has shown a picture.
///
/// Only the startup gate sets this: it needs the process to finish so it can
/// read the measurement, and a window waiting for a human would hang it.
pub fn exit_after_first_frame() -> bool {
    std::env::var_os("NITID_EXIT_AFTER_FIRST_FRAME").is_some_and(|value| value != "0")
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

/// How long the first picture took to appear, once it has.
#[cfg(test)]
pub fn elapsed() -> Option<Duration> {
    FIRST_PIXELS.get().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

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
