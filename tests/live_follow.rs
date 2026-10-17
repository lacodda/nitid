//! The release gate for the folder watch: **does the open window notice?**
//!
//! The unit tests in `src/folder.rs` hold the listing to account when it is
//! read again, and they pass just as well with nothing ever reading it again:
//! the watch is started from the event loop after the first frame, and a
//! deleted line there would leave every unit test green and the feature dead.
//! So this gate runs the real viewer on a real folder and watches its window:
//! a picture written into the folder joins the count in the title, `N` puts
//! the newest up, the next picture written goes up by itself, and the picture
//! on screen deleted from outside gives way to the one before it.
//!
//! The key is posted to the window as a message, the way the viewer is driven
//! where there is no desktop to type at, and the title is read back with the
//! same call Explorer's taskbar uses. Windows-only, because both are.

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

/// How long the window is given to show a change. The watch waits a quarter
/// of a second for the folder to go quiet; the rest is a slow test machine.
const PATIENCE: Duration = Duration::from_secs(20);

fn viewer() -> PathBuf {
    let mut dir = std::env::current_exe().expect("the test's own path");
    dir.pop();
    if dir.ends_with("deps") {
        dir.pop();
    }
    let exe = dir.join("nitid.exe");
    assert!(exe.exists(), "{} is not built", exe.display());
    exe
}

/// A picture written by a third-party encoder, a shade of its own so no two
/// are the same file.
fn picture(path: &Path, shade: u8) {
    let image = image::RgbImage::from_pixel(48, 32, image::Rgb([shade, 255 - shade, 128]));
    image.save(path).expect("writing a picture");
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

/// Press a letter key on the window: down and up, with the scan code a US
/// keyboard gives it, which is what turns the key into its letter.
fn press(window: HWND, virtual_key: u8, scan_code: u32) {
    let down = 1 | (scan_code << 16);
    let up = down | (1 << 30) | (1 << 31);
    // SAFETY: posting to a window of a process this test started.
    unsafe {
        PostMessageW(Some(window), WM_KEYDOWN, WPARAM(usize::from(virtual_key)), LPARAM(down as isize)).expect("posting the key");
        PostMessageW(Some(window), WM_KEYUP, WPARAM(usize::from(virtual_key)), LPARAM(up as isize)).expect("posting the key");
    }
}

/// The viewer, closed and waited for however the test ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(window) = window_of(self.0.id()) {
            // SAFETY: as above.
            let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.0.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_open_window_follows_the_folder() {
    let folder = tempfile::tempdir().expect("a folder to watch");
    // The viewer writes its settings when it closes; they go here, not into
    // the settings of whoever runs the test.
    let settings = tempfile::tempdir().expect("a place for settings");
    let first = folder.path().join("a.png");
    picture(&first, 10);

    let child = common::keep_data_in(&mut Command::new(viewer()), settings.path())
        .arg(&first)
        .env("NITID_NO_SINGLE_INSTANCE", "1")
        .spawn()
        .expect("starting the viewer");
    let running = Running(child);

    let deadline = Instant::now() + PATIENCE;
    let window = loop {
        if let Some(window) = window_of(running.0.id()) {
            break window;
        }
        assert!(Instant::now() < deadline, "the viewer never showed a window");
        std::thread::sleep(Duration::from_millis(50));
    };
    title_saying(window, "a.png");

    // A picture written into the folder joins the walk — and, with nothing
    // followed, the window stays on the picture it was showing.
    std::thread::sleep(Duration::from_millis(1100));
    picture(&folder.path().join("b.png"), 90);
    title_saying(window, "a.png — 1 of 2");

    // N: the newest picture, now.
    press(window, b'N', 0x31);
    title_saying(window, "b.png — 2 of 2");

    // And the next one written goes up by itself.
    std::thread::sleep(Duration::from_millis(1100));
    picture(&folder.path().join("c.png"), 170);
    title_saying(window, "c.png — 3 of 3");

    // The picture on screen deleted from outside gives way to the one before
    // it — it was the last of the folder, so there is no next one.
    std::fs::remove_file(folder.path().join("c.png")).expect("deleting the picture on screen");
    title_saying(window, "b.png — 2 of 2");

    drop(running);
}
