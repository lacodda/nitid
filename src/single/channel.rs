//! The named pipe the instances talk over, on Windows.
//!
//! Creating a named pipe instance is atomic: two processes racing to be the
//! window both call `CreateNamedPipeW`, and exactly one succeeds. That is the
//! whole election — no lock file to go stale, no mutex to leak if a process is
//! killed, and the pipe disappears with the process that owned it.
//!
//! The owner listens on a background thread and wakes the event loop through
//! the proxy it was given, which is the same route a finished decode takes.
//! Nothing polls: the promise that a still image costs no wakeups (v0.12.0)
//! survives.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WriteFile};
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::core::HSTRING;

/// How much of a message the pipe buffers, and the ceiling on one read.
///
/// A selection of a few thousand paths is well under this; the decoder is
/// still free to refuse a message that claims more (see the parent module).
const BUFFER: u32 = 1024 * 1024;

/// The largest message a messenger will send.
///
/// Half the pipe's buffer, and the reason is not tidiness. A write into a
/// named pipe blocks until somebody drains it, so a message larger than the
/// buffer hangs the messenger for as long as the window fails to read —
/// measured, and it hangs for ever if the window never reads at all. Keeping
/// every message inside the buffer means the write always completes and the
/// messenger always gets to exit.
///
/// A path is at most 64 KiB (see the parent module), so this still carries
/// several thousand of them — far past any real selection.
const MAX_MESSAGE: usize = BUFFER as usize / 2;

/// A pipe handle that closes itself.
struct Pipe(HANDLE);

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateNamedPipeW` or `CreateFileW` and
        // is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: a pipe handle is not tied to the thread that made it, and this owns
// it exclusively.
unsafe impl Send for Pipe {}

/// Try to become the process that owns the window.
///
/// `Ok(Some(listener))` when this process created the pipe and is therefore
/// the owner; `Ok(None)` when somebody else already holds it. An error means
/// the pipe could not be created for a reason other than it already existing,
/// in which case the caller opens a window anyway — failing to share is not a
/// reason to refuse to show a picture.
pub fn claim(name: &str) -> Result<Option<Listener>> {
    match create(name) {
        Ok(pipe) => Ok(Some(Listener { pipe })),
        Err(Refused::Taken) => Ok(None),
        Err(Refused::Failed(error)) => Err(anyhow::Error::new(error).context("creating the instance pipe")),
    }
}

/// Why a claim did not succeed.
enum Refused {
    /// Somebody else owns the pipe — the ordinary case for every launch
    /// after the first.
    Taken,
    /// Anything else, which the caller reports rather than swallows.
    Failed(windows::core::Error),
}

fn create(name: &str) -> std::result::Result<Pipe, Refused> {
    // `CreateNamedPipeW` signals failure with `INVALID_HANDLE_VALUE` rather
    // than a `Result`, so the error has to be fetched separately.
    //
    // SAFETY: the name is a NUL-terminated wide string for the lifetime of the
    // call, and a valid handle is wrapped so it is closed exactly once.
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            // One instance at a time: being unable to create a second one is
            // exactly the signal that another process is the window.
            1,
            BUFFER,
            BUFFER,
            0,
            None,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        // SAFETY: read immediately after the failed call, on this thread.
        let code = unsafe { GetLastError() };
        return Err(if code == ERROR_PIPE_BUSY || code == ERROR_ACCESS_DENIED {
            Refused::Taken
        } else {
            Refused::Failed(windows::core::Error::from(code.to_hresult()))
        });
    }

    Ok(Pipe(handle))
}

/// The owner's end: accepts one message at a time, forever.
pub struct Listener {
    pipe: Pipe,
}

impl Listener {
    /// Block until a messenger connects, and return what it said.
    ///
    /// `None` when the message was not one of ours, which the caller ignores
    /// rather than treating as fatal: something else on the machine is free to
    /// connect to a pipe, and the window carries on regardless.
    fn accept(&self) -> Result<Option<Vec<PathBuf>>> {
        // SAFETY: the handle is live for the duration of the call.
        let connected = unsafe { ConnectNamedPipe(self.pipe.0, None) };
        if let Err(error) = connected {
            let code = error.code();
            // Two failures here are not failures at all, and taking either of
            // them for one stops the window ever accepting a file again:
            //
            // - `ERROR_PIPE_CONNECTED`: a messenger arrived between creating
            //   the pipe and this call. It is already connected.
            // - `ERROR_NO_DATA`: a messenger connected and closed without
            //   saying anything. Measured — anything on the machine may open
            //   a pipe it finds, and one that hangs up must cost nothing more
            //   than the next `DisconnectNamedPipe`.
            let harmless = code == ERROR_PIPE_CONNECTED.to_hresult() || code == ERROR_NO_DATA.to_hresult();
            if !harmless {
                // SAFETY: the handle is live; readying it for the next
                // messenger before reporting keeps one bad connection from
                // being the last one this window ever takes.
                unsafe {
                    let _ = DisconnectNamedPipe(self.pipe.0);
                }
                return Err(error).context("waiting for another instance");
            }
            if code == ERROR_NO_DATA.to_hresult() {
                // SAFETY: as above — reset and wait for somebody with
                // something to say.
                unsafe { DisconnectNamedPipe(self.pipe.0) }.context("releasing the instance pipe")?;
                return Ok(None);
            }
        }

        let message = self.read();

        // SAFETY: the handle is live; disconnecting readies it for the next
        // messenger. A failure here would leave the pipe unusable, which the
        // caller sees as the listen loop ending.
        unsafe { DisconnectNamedPipe(self.pipe.0) }.context("releasing the instance pipe")?;

        Ok(message.and_then(|bytes| super::decode(&bytes)))
    }

    /// Read one message to end of stream.
    ///
    /// `None` on any read failure: the sender is another process and may die
    /// mid-message, which is its business, not a fault of this one.
    fn read(&self) -> Option<Vec<u8>> {
        let mut message = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            // SAFETY: `chunk` outlives the call and `read` receives the count.
            let result = unsafe { ReadFile(self.pipe.0, Some(&mut chunk), Some(&mut read), None) };
            match result {
                // Zero bytes means the writer closed its end: the message is
                // complete.
                Ok(()) if read == 0 => return Some(message),
                Ok(()) => {
                    if message.len() + read as usize > BUFFER as usize {
                        return None;
                    }
                    message.extend_from_slice(&chunk[..read as usize]);
                }
                // `ERROR_BROKEN_PIPE` is the ordinary end of a message too.
                Err(error) if error.code() == ERROR_BROKEN_PIPE.to_hresult() => return Some(message),
                Err(_) => return None,
            }
        }
    }

    /// Hand every message to `deliver` until the pipe fails.
    ///
    /// Runs on its own thread; `deliver` is what wakes the event loop.
    pub fn listen(self, deliver: impl Fn(Vec<PathBuf>) + Send + 'static) {
        std::thread::spawn(move || {
            loop {
                match self.accept() {
                    Ok(Some(paths)) => deliver(paths),
                    // A message that was not ours: ignored, and the next one
                    // is still served.
                    Ok(None) => {}
                    // The pipe is gone — the process is usually on its way
                    // out. Stop rather than spin on a broken handle.
                    Err(_) => return,
                }
            }
        });
    }
}

/// Hand `paths` to the process that owns `name`.
///
/// Fails when nobody is listening, which the caller treats as "open a window
/// after all" rather than as an error worth showing.
pub fn send(name: &str, paths: &[PathBuf], patience: std::time::Duration) -> Result<()> {
    // The whole hand-over runs on a thread this one gives up on, because
    // every step of it depends on a process this one does not control. A
    // window that has the pipe but has stopped reading — a wedged instance,
    // or a build where the listener never started — would otherwise hold this
    // process open for ever, and a messenger that never exits is worse than
    // one that opens a second window.
    let name = name.to_string();
    let paths = paths.to_vec();
    let (report, outcome) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = report.send(deliver(&name, &paths, patience));
    });

    match outcome.recv_timeout(patience) {
        Ok(result) => result,
        // The thread is left to finish or die with the process; it holds only
        // its own pipe handle.
        Err(_) => bail!("the open window did not take the file in time"),
    }
}

fn deliver(name: &str, paths: &[PathBuf], patience: std::time::Duration) -> Result<()> {
    let message = super::encode(paths);
    // Refused before connecting rather than after: a message this large would
    // block the write until a reader drained it, and a window that is wedged
    // would hang this process for ever. Opening a window of our own is the
    // better answer, and that is what the caller does with this error.
    if message.len() > MAX_MESSAGE {
        bail!("too many files to hand over at once ({} bytes)", message.len());
    }

    let pipe = open_waiting(name, patience)?;

    let mut written = 0;
    while written < message.len() {
        let mut count = 0u32;
        // SAFETY: the slice outlives the call and `count` receives the number
        // of bytes taken.
        unsafe { WriteFile(pipe.0, Some(&message[written..]), Some(&mut count), None) }.context("handing the file to the open window")?;
        if count == 0 {
            bail!("the open window stopped listening mid-message");
        }
        written += count as usize;
    }

    Ok(())
}

/// Connect, waiting out a listener that is busy with someone else.
///
/// The two failures a messenger can meet are not the same thing, and the
/// difference was measured rather than assumed:
///
/// - `ERROR_FILE_NOT_FOUND` (2) — nobody owns the pipe. Give up at once: this
///   process should open its own window rather than wait for one.
/// - `ERROR_PIPE_BUSY` (231) — a window is there but its single instance is
///   serving another messenger. Wait and try again; giving up here is what
///   would open a second window when five files are selected at once.
///
/// `WaitNamedPipeW` looks like the answer to the second and is not: measured
/// against a busy one-instance pipe it returns false immediately, because the
/// instance is connected rather than pending. Retrying is what works.
fn open_waiting(name: &str, patience: std::time::Duration) -> Result<Pipe> {
    let deadline = std::time::Instant::now() + patience;
    loop {
        match open(name) {
            Ok(pipe) => return Ok(pipe),
            Err(code) if code == ERROR_PIPE_BUSY.0 && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(code) if code == ERROR_FILE_NOT_FOUND.0 => bail!("no window is open to hand the file to"),
            Err(code) => bail!("could not reach the open window (error {code})"),
        }
    }
}

/// One attempt to connect. `Err` carries the raw Windows error code, because
/// the caller's decision turns on exactly which one it is.
fn open(name: &str) -> std::result::Result<Pipe, u32> {
    // SAFETY: the name is a NUL-terminated wide string for the call, and the
    // handle is wrapped so it is closed exactly once.
    let handle = unsafe {
        CreateFileW(
            &HSTRING::from(name),
            GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    };

    match handle {
        Ok(handle) if handle != INVALID_HANDLE_VALUE => Ok(Pipe(handle)),
        // SAFETY: read immediately after the failed call, on this thread.
        _ => Err(unsafe { GetLastError() }.0),
    }
}

/// Whether somebody is listening on `name`.
///
/// Not used by the viewer itself — a second instance either hands its file
/// over or opens a window, and asking first would only add a round trip. The
/// tests need it to wait for the first viewer to be ready before starting a
/// second.
///
/// Connecting is the question: a listener that is free accepts, and one that
/// is busy answers `ERROR_PIPE_BUSY`, which is just as good an answer. The
/// connection is dropped immediately, which the listener sees as a messenger
/// that said nothing.
pub fn is_listening(name: &str) -> bool {
    match open(name) {
        Ok(_) => true,
        Err(code) => code == ERROR_PIPE_BUSY.0,
    }
}

/// Wait for a listener to appear, for a test that has just started one.
#[cfg(test)]
pub fn wait_until_listening(name: &str, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if is_listening(name) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

/// A listener that collects what it is sent, for tests.
#[cfg(test)]
pub fn collect(listener: Listener) -> std::sync::mpsc::Receiver<Vec<PathBuf>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    listener.listen(move |paths| {
        let _ = sender.send(paths);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A pipe name of this test's own, so a run never talks to the viewer the
    /// developer has open.
    fn unique_name(tag: &str) -> String {
        format!(r"\\.\pipe\nitid-test-{}-{tag}", std::process::id())
    }

    #[test]
    fn a_message_reaches_the_process_that_claimed_the_pipe() {
        let name = unique_name("delivery");
        let listener = claim(&name).expect("creating the pipe").expect("nobody else holds a fresh name");
        let received = collect(listener);
        // The listener's thread has to reach `ConnectNamedPipe` before a
        // messenger arrives, or the first connection lands on a pipe that is
        // created but not yet accepting.
        assert!(wait_until_listening(&name, Duration::from_secs(5)), "the listener never came up");

        let paths = vec![PathBuf::from(r"C:\pictures\one.jpg"), PathBuf::from(r"C:\pictures\two.png")];
        send(&name, &paths, Duration::from_secs(5)).expect("sending to a live listener");

        let delivered = received.recv_timeout(Duration::from_secs(5)).expect("the listener answered");
        assert_eq!(delivered, paths);
    }

    /// The election: the second claim must fail, or two windows open.
    #[test]
    fn only_one_process_can_claim_a_name() {
        let name = unique_name("election");
        let first = claim(&name).expect("creating the pipe").expect("the first claim wins");

        let second = claim(&name).expect("a taken name is not an error");
        assert!(second.is_none(), "two processes both believed they owned the window");

        drop(first);
    }

    /// Once the owner is gone the name is free again — otherwise closing the
    /// viewer would leave every later launch unable to open a window.
    #[test]
    fn a_name_is_free_again_once_its_owner_is_gone() {
        let name = unique_name("release");
        let first = claim(&name).expect("creating the pipe").expect("the first claim wins");
        drop(first);

        let second = claim(&name).expect("creating the pipe").expect("the name is free again");
        drop(second);
    }

    /// The invariant the messenger's liveness rests on: nothing it will
    /// agree to send can exceed what the pipe will hold without a reader.
    ///
    /// Measured the hard way — a write larger than the buffer blocks until
    /// somebody drains it, and against a window that never reads it blocks
    /// for ever. This is what keeps that impossible.
    #[test]
    fn no_message_can_be_larger_than_the_pipe_will_hold() {
        assert!(MAX_MESSAGE <= BUFFER as usize, "a message this size would block the write");
    }

    /// A selection too large to send must be refused rather than attempted,
    /// and refused at once — the caller opens its own window instead.
    #[test]
    fn an_enormous_selection_is_refused_before_it_can_block() {
        let name = unique_name("enormous");
        let listener = claim(&name).expect("creating the pipe").expect("the first claim wins");
        assert!(wait_until_listening(&name, Duration::from_secs(5)), "the listener never came up");
        let _received = collect(listener);

        // Long paths, enough of them to pass the ceiling.
        let long = "x".repeat(4096);
        let paths: Vec<PathBuf> = (0..200).map(|index| PathBuf::from(format!(r"C:\{long}\{index}.jpg"))).collect();

        let started = std::time::Instant::now();
        let outcome = send(&name, &paths, Duration::from_secs(5));
        assert!(outcome.is_err(), "an oversize selection was sent rather than refused");
        assert!(started.elapsed() < Duration::from_secs(1), "refusing took {:?}", started.elapsed());
    }

    #[test]
    fn sending_to_nobody_fails_rather_than_hanging() {
        let name = unique_name("nobody");
        let started = std::time::Instant::now();
        let outcome = send(&name, &[PathBuf::from("a.jpg")], Duration::from_secs(5));
        assert!(outcome.is_err(), "sending into a pipe nobody owns appeared to succeed");
        // "Nobody is there" has to be answered at once rather than waited out:
        // that delay would be paid on every ordinary cold start, which is the
        // very thing this version exists to make faster.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a missing window took {:?} to report",
            started.elapsed()
        );
    }

    /// Several messengers in a row, which is what multi-select looks like:
    /// the shell starts one process per file and they arrive one after
    /// another.
    #[test]
    fn a_listener_serves_one_messenger_after_another() {
        let name = unique_name("several");
        let listener = claim(&name).expect("creating the pipe").expect("the first claim wins");
        assert!(wait_until_listening(&name, Duration::from_secs(5)), "the listener never came up");
        let received = collect(listener);

        for index in 0..5 {
            let path = PathBuf::from(format!(r"C:\pictures\{index}.jpg"));
            // No retry loop here on purpose: waiting out a listener that is
            // busy with the previous messenger is `send`'s job, and a test
            // that retried by hand would hide it failing to.
            send(&name, std::slice::from_ref(&path), Duration::from_secs(5)).unwrap_or_else(|error| panic!("messenger {index}: {error:#}"));
        }

        let mut seen = Vec::new();
        for _ in 0..5 {
            let batch = received.recv_timeout(Duration::from_secs(5)).expect("every messenger is served");
            seen.extend(batch);
        }
        assert_eq!(seen.len(), 5, "not every messenger was heard: {seen:?}");
    }

    /// Rubbish on the pipe must not take the window down, and must not stop
    /// the next real message from arriving.
    #[test]
    fn a_foreign_message_is_ignored_and_the_next_one_still_arrives() {
        let name = unique_name("foreign");
        let listener = claim(&name).expect("creating the pipe").expect("the first claim wins");
        assert!(wait_until_listening(&name, Duration::from_secs(5)), "the listener never came up");
        let received = collect(listener);

        // Something that is not nitid, connecting to a pipe it found.
        {
            let pipe = open_waiting(&name, Duration::from_secs(5)).expect("a live listener accepts a connection");
            let rubbish = b"hello from something else";
            let mut count = 0u32;
            unsafe { WriteFile(pipe.0, Some(rubbish), Some(&mut count), None) }.expect("writing rubbish");
        }

        let path = vec![PathBuf::from(r"C:\pictures\real.jpg")];
        send(&name, &path, Duration::from_secs(5)).expect("the listener is still serving after rubbish");

        let delivered = received.recv_timeout(Duration::from_secs(5)).expect("the real message still arrived");
        assert_eq!(delivered, path);
    }
}
