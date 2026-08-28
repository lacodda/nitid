//! Decoding in a process that can do nothing else.
//!
//! An image decoder parses hostile input as its normal job: files arrive from
//! the internet, from messengers, from strangers. Pure-Rust decoders bound the
//! damage to a panic, but HEIC and AVIF exist only as C libraries, where the
//! same bug is arbitrary code execution. Those run here instead — in a child
//! process holding a token that can open no files, reach no network, and dies
//! with its parent. See `docs/adr/0002-sandbox-c-decoders.md`.
//!
//! The child is this same executable, re-launched with a hidden argument. One
//! binary means one thing to sign, one thing to update, and no way for the two
//! halves to fall out of step.
//!
//! The file crosses as bytes on stdin and comes back as pixels on stdout: the
//! child is never told a path, so a compromised decoder cannot ask for a
//! different file than the one the user opened.
//!
//! What comes back is the whole of what the decoder learned — pixels, the
//! orientation to apply, and the colour profile if there is one. Reading the
//! last two in the parent would work for formats that state them in the
//! container and quietly lose them for AVIF, where the colour description
//! lives in the bitstream and only the decoder ever sees it.

// The protocol is written and tested everywhere, but only the Windows build
// has a process on the other end of it: elsewhere `decode` runs in-process, so
// the halves that speak to a child go unused. That is a real statement about
// the platform rather than something to silence one function at a time.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod protocol;

#[cfg(windows)]
mod section;
#[cfg(windows)]
mod spawn;
#[cfg(windows)]
mod windows;

use std::io::{self, Read, Write};

use anyhow::{Context, Result};

use crate::format::Format;
use crate::image_source::LoadedImage;

/// The argument that turns this executable into a decoder.
///
/// Spelled with leading dashes so it cannot collide with a file name: every
/// other unrecognised argument is treated as a path to open, because a file
/// may legitimately be called `install`.
pub const DECODE_ARGUMENT: &str = "--decode-stdin";

/// How long a decode may take before the child is killed.
///
/// A decoder that has not answered in half a minute is either wedged on a
/// crafted file or looping; either way the viewer must not wait for it.
#[cfg(windows)]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The timeout, or a shorter one asked for through the environment.
///
/// Overridable so the timeout can be tested against a decoder that really
/// hangs, in a second rather than in thirty. Without this the code that kills
/// a wedged decoder would ship unexercised — which is the state it was in
/// from v0.6.0 until now.
#[cfg(windows)]
fn timeout() -> std::time::Duration {
    std::env::var("NITID_DECODE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(TIMEOUT)
}

/// Decode `bytes` in a sandboxed child process.
///
/// A crash, a hang, or a refusal in the child all come back as an ordinary
/// error: the viewer says the file would not open and stays alive, which is
/// the entire point of the boundary.
#[cfg(windows)]
pub fn decode(bytes: &[u8], format: Format) -> Result<LoadedImage> {
    windows::decode(bytes, format, timeout())
}

/// Bind `port` on loopback and report whether any connection arrives within
/// two seconds. The other half of this handshake is the test, which hammers
/// the port from outside the sandbox for the same window.
fn connection_arrives_on(port: &str) -> bool {
    let Ok(listener) = std::net::TcpListener::bind(format!("127.0.0.1:{port}")) else {
        return false;
    };
    if listener.set_nonblocking(true).is_err() {
        return false;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if listener.accept().is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Remove the decoder's container profile, as part of uninstalling.
#[cfg(windows)]
pub fn remove_container_profile() -> Result<()> {
    spawn::delete_profile()
}

/// Whether this process holds an AppContainer token.
///
/// Asked by the probe so the tests can assert *which* confinement answered:
/// "the network was closed" only counts as the container's doing if the
/// process can show it was in one.
#[cfg(windows)]
fn running_in_container() -> bool {
    use ::windows::Win32::Foundation::{CloseHandle, HANDLE};
    use ::windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenIsAppContainer};
    use ::windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: the token handle is opened on this very process and closed
    // before returning; the out-buffer is a plain local.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut value = 0u32;
        let mut length = 0u32;
        let asked = GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some(&raw mut value as *mut core::ffi::c_void),
            size_of::<u32>() as u32,
            &mut length,
        );
        let _ = CloseHandle(token);
        asked.is_ok() && value != 0
    }
}

#[cfg(not(windows))]
fn running_in_container() -> bool {
    false
}

/// Off Windows there is no sandbox yet, so the decode happens in-process.
///
/// nitid ships for Windows; this keeps the Linux CI build honest rather than
/// pretending to a confinement it does not have.
#[cfg(not(windows))]
pub fn decode(bytes: &[u8], format: Format) -> Result<LoadedImage> {
    let mut loaded = decode_in_this_process(bytes)?;
    loaded.format = format;
    Ok(loaded)
}

/// Which executable to launch as the decoder.
///
/// Always the console binary, never the windowed one. A GUI-subsystem process
/// starts with no standard handles at all, so `nitidw.exe` asked to decode
/// would have nowhere to write the pixels — see
/// `docs/adr/0004-two-binaries-console-and-windowed.md`. The two are built and
/// installed together, so the sibling is always there.
///
/// Overridable through `NITID_DECODER` so the tests can point at a binary that
/// answers the protocol; nothing in normal use sets it.
#[cfg(windows)]
fn decoder_executable() -> Result<std::path::PathBuf> {
    if let Some(path) = std::env::var_os("NITID_DECODER") {
        return Ok(path.into());
    }

    let mut exe = std::env::current_exe().context("finding this executable")?;
    exe.set_file_name(if cfg!(windows) { "nitid.exe" } else { "nitid" });
    Ok(exe)
}

/// Run as the decoder: read a file on stdin, write pixels on stdout.
///
/// This is the whole of the child process. It never returns an error to the
/// caller — a file that will not decode is reported down the pipe, because the
/// viewer needs the reason and an exit code cannot carry one.
pub fn run_as_decoder() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).context("reading the request")?;

    // A decoder that never answers, on request. This exists so the timeout can
    // be tested against a real wedged child rather than a stand-in: the code
    // that kills one is exactly the code that must not be assumed to work.
    // Nothing in normal use sets this.
    if std::env::var_os("NITID_DECODER_HANGS").is_some() {
        loop {
            std::thread::park();
        }
    }

    let (request, section_handle) = protocol::read_request(&input[..]).context("reading the request")?;

    let stdout = io::stdout();
    let mut out = stdout.lock();

    // A decoder taught to try the network, on request. This is how "the
    // container closes the network" is a measurement rather than a belief —
    // the same probe is what proved a low integrity token does *not* close
    // it. Two shapes: an address means "try to reach it" (the exfiltration
    // direction), and `accept:PORT` means "listen on that port and say
    // whether anyone got through" (the command-and-control direction; binding
    // succeeds even inside a container, so the honest question is whether a
    // connection ever arrives). Nothing in normal use sets this.
    if let Ok(target) = std::env::var("NITID_DECODER_PROBES_NETWORK") {
        let report = if let Some(port) = target.strip_prefix("accept:") {
            format!("network probe: container={} accepted={}", running_in_container(), connection_arrives_on(port))
        } else {
            let listen = std::net::TcpListener::bind("127.0.0.1:0").is_ok();
            let connect = target
                .parse()
                .ok()
                .map(|address| std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3)).is_ok())
                .unwrap_or(false);
            format!("network probe: container={} listen={listen} connect={connect}", running_in_container())
        };
        protocol::write_failure(&mut out, &report).context("answering the probe")?;
        out.flush().context("flushing the reply")?;
        return Ok(());
    }

    match decode_in_this_process(&request) {
        Ok(loaded) => {
            let image = protocol::RawImage {
                width: loaded.image.width,
                height: loaded.image.height,
                depth: match loaded.image.depth {
                    crate::image_source::Depth::Eight => 8,
                    crate::image_source::Depth::Sixteen => 16,
                },
                pixels: loaded.image.pixels,
                orientation: loaded.orientation.to_exif(),
                // A profile that will not re-encode is sent as none rather
                // than failing the decode: the picture is worth more than the
                // colour management on it.
                profile: loaded.profile.and_then(|profile| profile.encode().ok()).unwrap_or_default(),
            };
            answer_with_image(&mut out, &image, section_handle)
        }
        Err(error) => protocol::write_failure(&mut out, &format!("{error:#}")),
    }
    .context("answering the request")?;

    out.flush().context("flushing the reply")?;
    Ok(())
}

/// Send the image back — through the shared section when the viewer offered
/// one, inline down the pipe when it did not or the section will not take
/// the pixels.
///
/// The fallback is not an edge case to apologise for: it is the whole reply
/// path on a viewer that could not create a section, and it is what the
/// protocol tests exercise directly.
#[cfg(windows)]
fn answer_with_image(out: impl io::Write, image: &protocol::RawImage, section_handle: u64) -> Result<()> {
    if let Some(mut view) = section::DecoderView::open(section_handle)
        && view.write(&image.pixels).is_ok()
    {
        return protocol::write_shared_image(out, image);
    }

    // A test lever: with this set, falling back would let a broken section
    // path hide behind the pipe — every decode would still succeed and no
    // test could tell the difference. Refusing instead makes "the pixels
    // crossed through the section" an assertable fact. Nothing in normal use
    // sets this.
    if std::env::var_os("NITID_SHM_REQUIRED").is_some() {
        return protocol::write_failure(out, "the section was required but could not be written");
    }

    protocol::write_image(out, image)
}

/// Off Windows there is no child and no section; the inline path is the only
/// one.
#[cfg(not(windows))]
fn answer_with_image(out: impl io::Write, image: &protocol::RawImage, _section_handle: u64) -> Result<()> {
    protocol::write_image(out, image)
}

/// Decode without a sandbox, in whichever process calls this.
///
/// In the child that is the point; in the viewer it is the fallback for
/// platforms without a sandbox.
fn decode_in_this_process(bytes: &[u8]) -> Result<LoadedImage> {
    crate::image_source::decode_here(bytes)
}
