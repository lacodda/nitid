//! Being told when the open folder changes.
//!
//! A thread waits on Windows' change notification for the folder and says
//! "something changed" — nothing more. Which file arrived, left or was written
//! again is worked out by reading the folder again (`Folder::rescan`): the
//! notification is a doorbell, not a list, and a list kept in step with a
//! stream of per-file events is a second account of the folder that can drift
//! from the first.
//!
//! The thread sleeps in the kernel between changes, so a still picture in a
//! quiet folder still costs no wake-ups — the promise the event loop is built
//! on (see `app`).

use std::path::{Path, PathBuf};

use anyhow::Result;

/// A folder being listened to. Dropping it stops the listening.
pub struct Watch {
    dir: PathBuf,
    /// Held for what dropping it does: stopping the thread.
    #[cfg(windows)]
    _listener: imp::Listener,
}

impl Watch {
    /// Start listening to `dir`, calling `changed` from another thread every
    /// time something in it changes.
    ///
    /// `changed` may be called many times for one change — Windows reports a
    /// file being written as it is written — and the caller is expected to
    /// wait for the folder to go quiet before reading it.
    pub fn start(dir: &Path, changed: impl Fn() + Send + 'static) -> Result<Self> {
        #[cfg(windows)]
        let listener = imp::Listener::start(dir, changed)?;
        #[cfg(not(windows))]
        let _ = changed;

        Ok(Self {
            dir: dir.to_path_buf(),
            #[cfg(windows)]
            _listener: listener,
        })
    }

    /// The folder being listened to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use std::thread::JoinHandle;

    use anyhow::{Context, Result};
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::Storage::FileSystem::{
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification, FindFirstChangeNotificationW,
        FindNextChangeNotification,
    };
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
    use windows::core::HSTRING;

    /// The thread, and the event that tells it to stop.
    pub struct Listener {
        stop: HANDLE,
        thread: Option<JoinHandle<()>>,
    }

    impl Listener {
        pub fn start(dir: &Path, changed: impl Fn() + Send + 'static) -> Result<Self> {
            // Names created, deleted and renamed; a file written and grown.
            // Not attributes or security: a mark written into a file changes
            // its contents, and nothing else a viewer shows depends on them.
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE;
            // SAFETY: the path is a valid wide string for the duration of the
            // call; the handle is owned by the thread below, which closes it.
            let change = unsafe { FindFirstChangeNotificationW(&HSTRING::from(dir.as_os_str()), false, filter) }
                .with_context(|| format!("listening to {}", dir.display()))?;
            // SAFETY: an unnamed manual-reset event with default security.
            let stop = match unsafe { CreateEventW(None, true, false, None) } {
                Ok(stop) => stop,
                Err(error) => {
                    // SAFETY: the handle was returned by the call above.
                    let _ = unsafe { FindCloseChangeNotification(change) };
                    return Err(error).context("making the event that stops the listening");
                }
            };

            // Handles are pointers to the compiler, and pointers do not cross
            // threads; these are kernel handles, which do, so they travel as
            // numbers.
            let (change_raw, stop_raw) = (change.0 as isize, stop.0 as isize);
            let thread = std::thread::Builder::new()
                .name("folder watch".into())
                .spawn(move || {
                    let change = HANDLE(change_raw as *mut _);
                    let stop = HANDLE(stop_raw as *mut _);
                    loop {
                        // SAFETY: both handles stay open until this loop ends —
                        // the stop event until `drop` has joined this thread.
                        let woke = unsafe { WaitForMultipleObjects(&[change, stop], false, INFINITE) };
                        if woke != WAIT_OBJECT_0 {
                            // The stop event, or a wait that failed: either way
                            // there is nothing more to listen for.
                            break;
                        }
                        changed();
                        // SAFETY: the change handle is the one this thread owns.
                        if unsafe { FindNextChangeNotification(change) }.is_err() {
                            // The folder itself went — deleted, or its drive
                            // taken out. Nothing is left to report on.
                            break;
                        }
                    }
                    // SAFETY: closed once, by its only owner.
                    let _ = unsafe { FindCloseChangeNotification(change) };
                })
                .context("starting the folder watch")?;

            Ok(Self { stop, thread: Some(thread) })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            // SAFETY: the event is open until it is closed below, after the
            // thread that waits on it has finished.
            unsafe {
                let _ = SetEvent(self.stop);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            // SAFETY: the thread is gone, so nothing else holds this handle.
            unsafe {
                let _ = CloseHandle(self.stop);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A picture written into the folder rings the bell, and dropping the
    /// watch stops the thread rather than leaving it waiting for ever.
    #[test]
    fn a_file_written_into_the_folder_is_noticed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (tell, heard) = mpsc::channel();
        let watch = Watch::start(dir.path(), move || {
            let _ = tell.send(());
        })
        .expect("listening to the folder");
        assert_eq!(watch.dir(), dir.path());

        std::fs::write(dir.path().join("new.png"), b"a picture").unwrap();
        heard
            .recv_timeout(Duration::from_secs(5))
            .expect("writing a file into the folder was not noticed");

        // Dropping joins the thread: if the stop event did not reach it, this
        // would hang and the test runner would say so.
        drop(watch);
    }

    #[test]
    fn a_folder_that_is_not_there_is_an_error_not_a_silent_watch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let missing = dir.path().join("not-here");
        assert!(Watch::start(&missing, || {}).is_err());
    }
}
