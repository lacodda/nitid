//! One window, exercised with real processes.
//!
//! The point of this version is what happens *between* two launches, so a
//! unit test cannot show it: the thing under test is a second `nitid.exe`
//! deciding not to open a window. These start the real binary, the same one
//! a user runs.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// The binary under test, built by cargo alongside this test.
fn viewer() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nitid"))
}

/// A channel name nothing else will use.
///
/// Without this the tests would talk to whatever viewer the developer has
/// open — and, worse, would pass or fail depending on that.
fn instance_id(tag: &str) -> String {
    format!("test{}{tag}", std::process::id())
}

/// A small PNG, written by the `image` crate rather than by nitid's own code.
fn picture(path: &Path, shade: u8) {
    let mut pixels = image::RgbImage::new(64, 48);
    for pixel in pixels.pixels_mut() {
        *pixel = image::Rgb([shade, shade / 2, 255 - shade]);
    }
    pixels.save(path).expect("writing the fixture");
}

/// Start a viewer that stays up, and wait until it is showing something.
///
/// The window is the process that claims the channel, so a test that raced it
/// would be testing the race rather than the hand-over.
fn open_window(id: &str, image: &Path) -> Child {
    let child = Command::new(viewer())
        .arg(image)
        .env("NITID_INSTANCE_ID", id)
        .spawn()
        .expect("starting the first viewer");

    // Readiness is asked of the channel rather than by launching another
    // viewer: a launch with no file opens an empty window and never exits,
    // which would hang the test rather than answer it.
    //
    // Twenty seconds is far longer than a debug-build start on loaded CI
    // hardware and short enough that a viewer which never listens — the shape
    // every mutation of this mechanism takes — fails the run promptly instead
    // of leaving it looking hung.
    assert!(
        wait_for(Duration::from_secs(20), || nitid::testing::instance_is_listening(id)),
        "the first viewer never started listening"
    );

    child
}

/// Try to hand `paths` to the window owning `id`, as a second launch would.
///
/// Waits with a deadline rather than with `output()`. A messenger that hands
/// its file over exits within milliseconds; one that opened a window instead
/// never exits at all, and `output()` would wait for it for ever — turning
/// "this mechanism is broken" into a run that merely looks stuck.
fn handed_over(id: &str, paths: &[&Path], deadline: Duration) -> Result<Duration, String> {
    let started = Instant::now();
    let mut command = Command::new(viewer());
    for path in paths {
        command.arg(path);
    }
    let mut child = command
        .env("NITID_INSTANCE_ID", id)
        .spawn()
        .map_err(|error| format!("running the second viewer: {error}"))?;

    while started.elapsed() < deadline {
        match child.try_wait().map_err(|error| format!("waiting for the second viewer: {error}"))? {
            Some(status) if status.success() => return Ok(started.elapsed()),
            Some(status) => return Err(format!("the second viewer failed: {status:?}")),
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    Err(format!(
        "the second viewer was still running after {deadline:?} — it opened a window instead of handing over"
    ))
}

fn wait_for(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if ready() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Kill a child and reap it, so a failing test leaves no window behind.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The heart of the version: a second launch must not become a second window.
#[test]
fn a_second_launch_hands_its_file_over_instead_of_opening_a_window() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let first = directory.path().join("first.png");
    let second = directory.path().join("second.png");
    picture(&first, 40);
    picture(&second, 200);

    let id = instance_id("handover");
    let window = Reaped(open_window(&id, &first));

    // The second launch exits of its own accord. If it had opened a window it
    // would still be running, and `output()` would never return.
    let elapsed = handed_over(&id, &[&second], Duration::from_secs(20)).expect("the second launch hands over and exits");

    // It must also be quick: the whole point is skipping the window and the
    // graphics device, measured at 190-340 ms of a cold start. A generous
    // ceiling, because this runs on a debug build on shared CI hardware —
    // it is here to catch "it opened a window after all", not to grade.
    assert!(elapsed < Duration::from_secs(20), "handing a file over took {elapsed:?}");

    // The window is still up: handing over must not have taken it down.
    let mut window = window;
    assert!(
        window.0.try_wait().expect("checking the window").is_none(),
        "the first window exited when handed a file"
    );
}

/// Multi-select: the shell starts one process per file, and they must
/// converge on one window rather than opening five.
#[test]
fn several_launches_at_once_all_reach_the_same_window() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let first = directory.path().join("first.png");
    picture(&first, 10);

    let others: Vec<PathBuf> = (0..4)
        .map(|index| {
            let path = directory.path().join(format!("other{index}.png"));
            picture(&path, 60 + index * 40);
            path
        })
        .collect();

    let id = instance_id("multi");
    let mut window = Reaped(open_window(&id, &first));

    // All four at once, as the shell launches them.
    let messengers: Vec<Child> = others
        .iter()
        .map(|path| {
            Command::new(viewer())
                .arg(path)
                .env("NITID_INSTANCE_ID", &id)
                .spawn()
                .expect("starting a messenger")
        })
        .collect();

    // Waited out with a deadline, not `wait()`: a messenger that opened a
    // window rather than handing over never exits, and this test would hang
    // instead of reporting that the mechanism is broken.
    for (index, mut messenger) in messengers.into_iter().enumerate() {
        let started = Instant::now();
        let status = loop {
            match messenger.try_wait().expect("waiting for a messenger") {
                Some(status) => break status,
                None if started.elapsed() > Duration::from_secs(20) => {
                    let _ = messenger.kill();
                    let _ = messenger.wait();
                    panic!("messenger {index} was still running after 20s — it opened its own window");
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        assert!(status.success(), "messenger {index} failed: {status:?}");
    }

    assert!(
        window.0.try_wait().expect("checking the window").is_none(),
        "the window exited while messengers were handing files over",
    );
}

/// With the lever set, a second launch opens its own window — which is what
/// the startup gate depends on, and what proves the lever is not decorative.
#[test]
fn the_lever_turns_sharing_off() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let image = directory.path().join("alone.png");
    picture(&image, 90);

    let id = instance_id("lever");
    let window = Reaped(open_window(&id, &image));

    // Told not to share, this launch opens a window of its own, shows the
    // picture, and exits on the first frame.
    let output = Command::new(viewer())
        .arg(&image)
        .env("NITID_INSTANCE_ID", &id)
        .env("NITID_NO_SINGLE_INSTANCE", "1")
        .env("NITID_STARTUP_REPORT", "1")
        .env("NITID_EXIT_AFTER_FIRST_FRAME", "1")
        .output()
        .expect("running the second viewer");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("gpu ready"),
        "with sharing off the second launch should have built its own device; stderr was:\n{stderr}",
    );

    drop(window);
}

/// A launch with no window to talk to must open one rather than doing
/// nothing — the ordinary cold start, and the case a hand-over must never
/// swallow.
#[test]
fn a_launch_with_nobody_listening_opens_a_window() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let image = directory.path().join("cold.png");
    picture(&image, 120);

    let output = Command::new(viewer())
        .arg(&image)
        .env("NITID_INSTANCE_ID", instance_id("cold"))
        .env("NITID_STARTUP_REPORT", "1")
        .env("NITID_EXIT_AFTER_FIRST_FRAME", "1")
        .output()
        .expect("running the viewer");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("first pixels in"), "a cold start showed no picture; stderr was:\n{stderr}");
}
