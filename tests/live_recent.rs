//! The start screen and the list behind it, with the real binary.
//!
//! Until v0.39 a viewer started with no file opened a window that never laid
//! out any interface at all — the one launch with nothing to show showed
//! nothing. Now it opens on the folders it has been to, and every picture it
//! shows is written into that list. Both halves are a matter of what one
//! process leaves behind for the next, so they are run as processes.
//!
//! Windows-only, like the start screen's file and the window it is drawn in.

#![cfg(windows)]

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The binary under test, built by cargo alongside this test.
fn viewer() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nitid"))
}

/// A small PNG, written by the `image` crate rather than by nitid's own code.
fn picture(path: &Path, shade: u8) {
    let mut pixels = image::RgbImage::new(64, 48);
    for pixel in pixels.pixels_mut() {
        *pixel = image::Rgb([shade, 255 - shade, shade / 2]);
    }
    pixels.save(path).expect("writing the fixture");
}

/// Where the viewer keeps its list, under the data folder a gate gave it.
fn recent_file(data: &Path) -> PathBuf {
    data.join("lacodda").join("nitid").join("recent")
}

/// Run the viewer to its own exit and return what it said, or kill it once
/// `patience` is spent.
///
/// A deadline rather than `output()`: the defect this file exists for is a
/// window that never lays anything out, and a viewer waiting for a frame that
/// never comes never exits either — the run would hang rather than fail.
fn run(command: &mut Command, patience: Duration) -> Result<String, String> {
    let mut child = command
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|error| format!("starting the viewer: {error}"))?;
    let mut stderr = child.stderr.take().expect("stderr was piped");
    let reader = std::thread::spawn(move || {
        let mut said = String::new();
        let _ = stderr.read_to_string(&mut said);
        said
    });

    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|error| format!("waiting for the viewer: {error}"))? {
            Some(_) => break,
            None if started.elapsed() > patience => {
                let _ = child.kill();
                let _ = child.wait();
                let said = reader.join().unwrap_or_default();
                return Err(format!("the viewer was still running after {patience:?}; it said:\n{said}"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Ok(reader.join().unwrap_or_default())
}

/// Started with nothing to open, the viewer lays out the start screen as its
/// first frame, with a tile for every folder it remembers that is still there.
#[test]
fn a_viewer_started_bare_opens_on_where_it_has_been() {
    let data = tempfile::tempdir().expect("a place for the viewer's own files");
    let pictures = tempfile::tempdir().expect("a place for pictures");
    let shoot = pictures.path().join("shoot");
    let screens = pictures.path().join("screens");
    for folder in [&shoot, &screens] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let first = shoot.join("a.png");
    let second = screens.join("b.png");
    picture(&first, 40);
    picture(&second, 200);

    // A folder on a drive that is not there today is remembered but not
    // shown: it is not a tile anybody can open.
    let gone = pictures.path().join("unplugged").join("c.png");
    let list = format!(
        "# written by the test\n1760000300\t{}\n1760000200\t{}\n1760000100\t{}\n",
        second.display(),
        gone.display(),
        first.display()
    );
    let file = recent_file(data.path());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, list).unwrap();

    let said = run(
        common::keep_data_in(&mut Command::new(viewer()), data.path())
            .env("NITID_NO_SINGLE_INSTANCE", "1")
            .env("NITID_STARTUP_REPORT", "1")
            .env("NITID_EXIT_AFTER_FIRST_FRAME", "interface"),
        Duration::from_secs(30),
    )
    .expect("a bare start never drew its first screen");

    if !said.contains("gpu ready") {
        eprintln!("skipping: the viewer could not open a window here:\n{said}");
        return;
    }
    assert!(
        said.contains("start screen drawn (tiles: 2)"),
        "the start screen did not show the two folders that are there:\n{said}"
    );
    assert!(
        said.contains("nitid: interface on screen at"),
        "the start screen was not the first frame:\n{said}"
    );
}

/// A picture shown is written into the list, so the next bare start has it.
#[test]
fn a_picture_shown_is_remembered_for_the_next_start() {
    let data = tempfile::tempdir().expect("a place for the viewer's own files");
    let pictures = tempfile::tempdir().expect("a place for pictures");
    let shown = pictures.path().join("seen.png");
    picture(&shown, 120);

    let said = run(
        common::keep_data_in(&mut Command::new(viewer()), data.path())
            .arg(&shown)
            .env("NITID_NO_SINGLE_INSTANCE", "1")
            .env("NITID_STARTUP_REPORT", "1")
            .env("NITID_EXIT_AFTER_FIRST_FRAME", "1"),
        Duration::from_secs(30),
    )
    .expect("the viewer never showed the picture");

    if !said.contains("first pixels in") {
        eprintln!("skipping: the viewer could not open a window here:\n{said}");
        return;
    }
    let list = std::fs::read_to_string(recent_file(data.path())).expect("the viewer wrote no list of where it has been");
    let remembered: Vec<&str> = list.lines().filter(|line| !line.starts_with('#')).collect();
    assert_eq!(remembered.len(), 1, "{list}");
    assert!(
        remembered[0].ends_with(&format!("\t{}", shown.display())),
        "the picture shown is not the one remembered:\n{list}"
    );
}
