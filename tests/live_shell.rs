//! The release gate for what Explorer starts: **a folder, and a look.**
//!
//! "Open with nitid" on a folder runs the viewer with the folder, and the
//! unit tests in `src/folder.rs` hold the listing it reads; they would pass
//! just as well with the window still opening the folder's parent, which is
//! what it did before. The space bar in Explorer reaches the viewer as a look,
//! and what a look is — a window the space bar sends away again — lives in
//! the event loop, where no unit test reaches. So this gate runs the real
//! viewer and reads its window's title, the way `live_follow` does, and
//! starts the real resident.
//!
//! Explorer itself is not driven here: pressing its space bar takes the
//! foreground from whoever is at the machine. What it is asked — what is
//! selected — is held by `resident::explorer`'s live test and by the release's
//! live run.

#![cfg(windows)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE, WM_KEYDOWN, WM_KEYUP,
};
use windows::core::BOOL;

/// How long the window is given to show a change, on a slow test machine.
const PATIENCE: Duration = Duration::from_secs(20);

fn binary(name: &str) -> PathBuf {
    let mut dir = std::env::current_exe().expect("the test's own path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let exe = dir.join(name);
    assert!(exe.exists(), "{} is not built", exe.display());
    exe
}

/// A picture written by a third-party encoder, a shade of its own.
fn picture(path: &Path, shade: u8) {
    let image = image::RgbImage::from_pixel(48, 32, image::Rgb([shade, 255 - shade, 128]));
    image.save(path).expect("writing a picture");
}

/// An instance of this test's own, so nothing here talks to the viewer — or
/// the resident — of whoever runs it.
fn instance_id(tag: &str) -> String {
    format!("shell{}{tag}", std::process::id())
}

/// The viewer's visible window, once it has one.
fn window_of(process: u32) -> Option<HWND> {
    struct Search {
        process: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(window: HWND, search: LPARAM) -> BOOL {
        // SAFETY: `search` is the struct below, alive for the enumeration.
        let search = unsafe { &mut *(search.0 as *mut Search) };
        let mut owner = 0u32;
        // SAFETY: plain queries about a window handle Windows just gave us.
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == search.process && unsafe { IsWindowVisible(window) }.as_bool() && title(window).ends_with("nitid") {
            search.found = Some(window);
            return false.into();
        }
        true.into()
    }
    let mut search = Search { process, found: None };
    // SAFETY: the callback only touches `search`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut _ as isize)) };
    search.found
}

fn title(window: HWND) -> String {
    let mut text = [0u16; 512];
    // SAFETY: the buffer is as long as it says.
    let length = unsafe { GetWindowTextW(window, &mut text) };
    String::from_utf16_lossy(&text[..length.max(0) as usize])
}

fn window_once_up(process: u32) -> HWND {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(window) = window_of(process) {
            return window;
        }
        assert!(Instant::now() < deadline, "the viewer never showed a window");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait for the title to say something, and return what it says.
fn title_saying(window: HWND, wanted: &str) -> String {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let now = title(window);
        if now.starts_with(wanted) {
            return now;
        }
        assert!(Instant::now() < deadline, "the title never said {wanted:?}; it says {now:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Press the space bar on the window: down and up, with its scan code.
fn press_space(window: HWND) {
    const SPACE: usize = 0x20;
    let down = 1 | (0x39 << 16);
    let up = down | (1 << 30) | (1 << 31);
    // SAFETY: posting to a window of a process this test started.
    unsafe {
        PostMessageW(Some(window), WM_KEYDOWN, WPARAM(SPACE), LPARAM(down as isize)).expect("posting the key");
        PostMessageW(Some(window), WM_KEYUP, WPARAM(SPACE), LPARAM(up as isize)).expect("posting the key");
    }
}

/// Wait for a process to end by itself, and say whether it did.
fn ends_within(child: &mut Child, patience: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// A process this test started, closed and waited for however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(window) = window_of(self.0.id()) {
            // SAFETY: as above.
            let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        if ends_within(&mut self.0, Duration::from_secs(10)).is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// A viewer started the way Explorer starts one, with its own instance and
/// its settings kept in `data`.
fn viewer(data: &Path, id: &str, arguments: &[&std::ffi::OsStr]) -> Command {
    let mut command = Command::new(binary("nitid.exe"));
    common::keep_data_in(&mut command, data).env("NITID_INSTANCE_ID", id).args(arguments);
    command
}

/// "Open with nitid" on a folder: the folder's own pictures, from the first
/// by name — not its parent's, and not whichever the disk listed first.
#[test]
fn a_folder_opens_on_its_first_picture() {
    let parent = tempfile::tempdir().expect("a temporary folder");
    let data = tempfile::tempdir().expect("a place for settings");
    picture(&parent.path().join("beside.png"), 5);
    let shoot = parent.path().join("shoot");
    std::fs::create_dir(&shoot).expect("creating a folder");
    for (name, shade) in [("c.png", 60), ("a.png", 120), ("b.png", 180)] {
        picture(&shoot.join(name), shade);
    }

    let id = instance_id("folder");
    let running = Running(viewer(data.path(), &id, &[shoot.as_os_str()]).spawn().expect("starting the viewer"));
    let window = window_once_up(running.0.id());
    title_saying(window, "a.png — 1 of 3");
}

/// A folder handed to a window already open takes the window over: it is a
/// place to walk, not one more file in the walk that was going on.
#[test]
fn a_folder_handed_over_takes_the_window_over() {
    let elsewhere = tempfile::tempdir().expect("a temporary folder");
    let shoot = tempfile::tempdir().expect("a temporary folder");
    let data = tempfile::tempdir().expect("a place for settings");
    picture(&elsewhere.path().join("x.png"), 30);
    for (name, shade) in [("b.png", 90), ("a.png", 150)] {
        picture(&shoot.path().join(name), shade);
    }

    let id = instance_id("handover");
    let first = elsewhere.path().join("x.png");
    let running = Running(viewer(data.path(), &id, &[first.as_os_str()]).spawn().expect("starting the viewer"));
    let window = window_once_up(running.0.id());
    title_saying(window, "x.png");

    let mut messenger = viewer(data.path(), &id, &[shoot.path().as_os_str()])
        .spawn()
        .expect("starting the second launch");
    let status = ends_within(&mut messenger, PATIENCE).expect("the second launch opened a window instead of handing over");
    assert!(status.success(), "the second launch failed: {status:?}");
    title_saying(window, "a.png — 1 of 2");
}

/// A folder with no picture in it opens on the folders the viewer has been
/// to, rather than on an empty canvas.
#[test]
fn a_folder_without_pictures_opens_the_start_screen() {
    let empty = tempfile::tempdir().expect("a temporary folder");
    let data = tempfile::tempdir().expect("a place for settings");
    std::fs::write(empty.path().join("notes.txt"), b"not a picture").expect("writing a file");

    let id = instance_id("empty");
    let running = Running(viewer(data.path(), &id, &[empty.path().as_os_str()]).spawn().expect("starting the viewer"));
    let window = window_once_up(running.0.id());
    title_saying(window, "Recent");
}

/// A look the space bar started is ended by the space bar: the window it
/// opened closes, and nothing of it stays behind.
#[test]
fn the_space_bar_closes_the_window_a_look_opened() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let data = tempfile::tempdir().expect("a place for settings");
    let looked_at = folder.path().join("a.png");
    picture(&looked_at, 70);
    picture(&folder.path().join("b.png"), 140);

    let id = instance_id("peek");
    let mut running = Running(
        viewer(data.path(), &id, &["--peek".as_ref(), looked_at.as_os_str()])
            .spawn()
            .expect("starting the viewer"),
    );
    let window = window_once_up(running.0.id());
    title_saying(window, "a.png — 1 of 2");

    press_space(window);
    let status = ends_within(&mut running.0, PATIENCE).expect("the space bar did not end the look");
    assert!(status.success(), "the viewer ended badly: {status:?}");
}

/// A look at a window that was already open does not close it — it was the
/// person's before the look — and once the look is over the space bar walks
/// the folder again, as it always has.
#[test]
fn a_look_at_an_open_window_leaves_it_open() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let data = tempfile::tempdir().expect("a place for settings");
    let first = folder.path().join("a.png");
    let looked_at = folder.path().join("b.png");
    picture(&first, 40);
    picture(&looked_at, 200);

    let id = instance_id("raised");
    let mut running = Running(viewer(data.path(), &id, &[first.as_os_str()]).spawn().expect("starting the viewer"));
    let window = window_once_up(running.0.id());
    title_saying(window, "a.png — 1 of 2");

    let mut messenger = viewer(data.path(), &id, &["--peek".as_ref(), looked_at.as_os_str()])
        .spawn()
        .expect("starting the look");
    let status = ends_within(&mut messenger, PATIENCE).expect("the look opened a window instead of handing over");
    assert!(status.success(), "the look failed: {status:?}");
    title_saying(window, "b.png — 2 of 2");

    // The first space ends the look and steps nowhere; the second walks. Had
    // the first one walked, two would have come back round to b.png.
    press_space(window);
    press_space(window);
    title_saying(window, "a.png — 1 of 2");
    assert!(
        running.0.try_wait().expect("asking after the viewer").is_none(),
        "the look closed a window it did not open"
    );
}

/// One resident per session: a second one started beside the first leaves at
/// once, and the first goes when told — which is what installing over it and
/// uninstalling it both rely on.
#[test]
fn one_resident_at_a_time_and_gone_when_told() {
    let id = instance_id("resident");
    let resident = || {
        Command::new(binary("nitidw.exe"))
            .arg("--resident")
            .env("NITID_INSTANCE_ID", &id)
            .spawn()
            .expect("starting the resident")
    };

    let mut first = Running(resident());
    let deadline = Instant::now() + PATIENCE;
    while !nitid::testing::resident_running(&id) {
        assert!(Instant::now() < deadline, "the resident never came up");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Guarded like the first: a second resident that stayed would otherwise
    // outlive the test holding its output, and the run would never end.
    let mut second = Running(resident());
    let status = ends_within(&mut second.0, PATIENCE).expect("a second resident stayed beside the first");
    assert!(status.success(), "the second resident failed: {status:?}");
    assert!(
        first.0.try_wait().expect("asking after the resident").is_none(),
        "the first resident went when a second came"
    );

    assert!(
        nitid::testing::stop_resident(&id).expect("telling the resident to go"),
        "no resident was found to stop"
    );
    let status = ends_within(&mut first.0, PATIENCE).expect("the resident did not go when told");
    assert!(status.success(), "the resident ended badly: {status:?}");
    assert!(!nitid::testing::resident_running(&id));
    assert!(!nitid::testing::stop_resident(&id).expect("asking again"), "a resident was stopped twice");
}
