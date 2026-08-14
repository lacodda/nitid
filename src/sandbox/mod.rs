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

pub mod protocol;

// The Windows implementation lands in the next commit of this stage.
// #[cfg(windows)]
// mod windows;

use std::io::{self, Read, Write};

use anyhow::{Context, Result};

use crate::image_source::DecodedImage;

/// The argument that turns this executable into a decoder.
///
/// Spelled with leading dashes so it cannot collide with a file name: every
/// other unrecognised argument is treated as a path to open, because a file
/// may legitimately be called `install`.
pub const DECODE_ARGUMENT: &str = "--decode-stdin";

/// Decode `bytes`, in a sandboxed child process where there is one.
pub fn decode(bytes: &[u8]) -> Result<DecodedImage> {
    decode_in_this_process(bytes)
}

/// Run as the decoder: read a file on stdin, write pixels on stdout.
///
/// This is the whole of the child process. It never returns an error to the
/// caller — a file that will not decode is reported down the pipe, because the
/// viewer needs the reason and an exit code cannot carry one.
pub fn run_as_decoder() -> Result<()> {
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).context("reading the request")?;

    let request = protocol::read_request(&input[..]).context("reading the request")?;

    let stdout = io::stdout();
    let mut out = stdout.lock();

    match decode_in_this_process(&request) {
        Ok(image) => protocol::write_image(
            &mut out,
            &protocol::RawImage {
                width: image.width,
                height: image.height,
                pixels: image.pixels,
            },
        ),
        Err(error) => protocol::write_failure(&mut out, &format!("{error:#}")),
    }
    .context("answering the request")?;

    out.flush().context("flushing the reply")?;
    Ok(())
}

/// Decode without a sandbox, in whichever process calls this.
///
/// In the child that is the point; in the viewer it is the fallback for
/// platforms without a sandbox.
fn decode_in_this_process(bytes: &[u8]) -> Result<DecodedImage> {
    crate::image_source::decode(bytes).map(|loaded| loaded.image)
}
