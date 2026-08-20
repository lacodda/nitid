//! What the viewer and the decoder process say to each other.
//!
//! The wire format is deliberately dull: fixed-width little-endian integers and
//! a length-prefixed blob, with no parser worth attacking. The decoder is the
//! process that reads hostile input, so the *reply* it sends is itself
//! untrusted — every field arriving from it is bounds-checked here rather than
//! believed.
//!
//! A request is: magic, version, length, then that many bytes of image file.
//! A reply is: magic, version, tag, then a payload the tag describes.
//!
//! The reply carries what the decoder learned about the file as well as its
//! pixels — the orientation to apply and the colour profile, if any. Reading
//! those in the parent instead would work for a format whose profile sits in
//! the container, and quietly lose it for one like AVIF, where the colour
//! description lives in the bitstream and only the decoder ever sees it.

use std::io::{Read, Write};

use anyhow::{Result, bail};

/// Four bytes that begin every message, so a desynchronised stream is caught
/// at once rather than read as a length of several gigabytes.
const MAGIC: [u8; 4] = *b"NTDS";

/// The protocol version, bumped when the shape of a message changes.
///
/// Both ends ship in the same executable and are updated together, so a
/// mismatch means something is impersonating one of them — refuse rather than
/// negotiate.
///
/// Version 2 added the orientation and the colour profile to an image reply.
const VERSION: u16 = 2;

/// The largest file the decoder will be handed, and the largest reply it may
/// send back.
///
/// Both directions are capped: a huge file would be refused before a decoder
/// ever sees it, and a compromised decoder cannot make the viewer allocate its
/// way out of memory by claiming an enormous image.
pub const MAX_PAYLOAD: usize = 512 * 1024 * 1024;

/// A decoded image as it crosses the process boundary.
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes of RGBA8.
    pub pixels: Vec<u8>,
    /// The orientation to apply, as EXIF numbers it (1 through 8).
    ///
    /// The decoder is the half that knows: some formats state the turn in the
    /// container and some in EXIF, and some decoders apply it themselves.
    pub orientation: u8,
    /// The ICC profile describing the pixels, if the decoder produced one.
    ///
    /// Empty when the image is untagged, which — following ADR 0005 — means
    /// it is shown as it is rather than converted from an assumed sRGB.
    pub profile: Vec<u8>,
}

/// The largest colour profile that will cross the boundary.
///
/// Real profiles run to a few kilobytes; a CMYK press profile with a large
/// lookup table might reach a megabyte. Past that, the decoder is either
/// broken or being used to make the viewer allocate.
pub const MAX_PROFILE: usize = 4 * 1024 * 1024;

/// Ask the decoder to decode `bytes`.
pub fn write_request(mut to: impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_PAYLOAD {
        bail!("the file is {} bytes, past the {MAX_PAYLOAD} this build will decode", bytes.len());
    }

    to.write_all(&MAGIC)?;
    to.write_all(&VERSION.to_le_bytes())?;
    to.write_all(&(bytes.len() as u64).to_le_bytes())?;
    to.write_all(bytes)?;
    to.flush()?;
    Ok(())
}

/// Read a request in the decoder process.
pub fn read_request(mut from: impl Read) -> Result<Vec<u8>> {
    expect_header(&mut from)?;

    let length = read_u64(&mut from)? as usize;
    if length > MAX_PAYLOAD {
        bail!("the request claims {length} bytes, past the {MAX_PAYLOAD} limit");
    }

    let mut bytes = vec![0u8; length];
    from.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Send a decoded image back to the viewer.
pub fn write_image(mut to: impl Write, image: &RawImage) -> Result<()> {
    if image.profile.len() > MAX_PROFILE {
        bail!("the profile is {} bytes, past the {MAX_PROFILE} this build will carry", image.profile.len());
    }

    to.write_all(&MAGIC)?;
    to.write_all(&VERSION.to_le_bytes())?;
    to.write_all(&[TAG_IMAGE])?;
    to.write_all(&image.width.to_le_bytes())?;
    to.write_all(&image.height.to_le_bytes())?;
    to.write_all(&[image.orientation])?;
    to.write_all(&(image.profile.len() as u64).to_le_bytes())?;
    to.write_all(&image.profile)?;
    to.write_all(&(image.pixels.len() as u64).to_le_bytes())?;
    to.write_all(&image.pixels)?;
    to.flush()?;
    Ok(())
}

/// Send a failure back to the viewer.
///
/// A file that will not decode is an ordinary outcome, not a crash: the
/// decoder says so and the viewer shows the reason.
pub fn write_failure(mut to: impl Write, message: &str) -> Result<()> {
    // Truncated at a generous bound so a decoder cannot answer a broken file
    // with an unbounded string.
    let message: String = message.chars().take(1024).collect();
    let bytes = message.as_bytes();

    to.write_all(&MAGIC)?;
    to.write_all(&VERSION.to_le_bytes())?;
    to.write_all(&[TAG_FAILURE])?;
    to.write_all(&(bytes.len() as u64).to_le_bytes())?;
    to.write_all(bytes)?;
    to.flush()?;
    Ok(())
}

/// Read whatever the decoder sent.
///
/// Everything here comes from the process that touched the hostile file, so
/// every number is checked before it is used to size an allocation.
pub fn read_reply(mut from: impl Read) -> Result<Result<RawImage, String>> {
    expect_header(&mut from)?;

    let mut tag = [0u8; 1];
    from.read_exact(&mut tag)?;

    match tag[0] {
        TAG_IMAGE => {
            let width = read_u32(&mut from)?;
            let height = read_u32(&mut from)?;

            let mut orientation = [0u8; 1];
            from.read_exact(&mut orientation)?;
            // Out-of-range values appear in real files; upright is the safe
            // reading, and the same one `Orientation::from_exif` takes.
            let orientation = orientation[0];

            let profile_length = read_u64(&mut from)? as usize;
            if profile_length > MAX_PROFILE {
                bail!("the decoder returned a {profile_length}-byte profile, past the {MAX_PROFILE} limit");
            }
            let mut profile = vec![0u8; profile_length];
            from.read_exact(&mut profile)?;

            let length = read_u64(&mut from)? as usize;

            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
                .unwrap_or(usize::MAX);
            if length != expected {
                bail!("the decoder returned {length} bytes for a {width}x{height} image, which needs {expected}");
            }
            if length > MAX_PAYLOAD {
                bail!("the decoder returned {length} bytes, past the {MAX_PAYLOAD} limit");
            }

            let mut pixels = vec![0u8; length];
            from.read_exact(&mut pixels)?;
            Ok(Ok(RawImage {
                width,
                height,
                pixels,
                orientation,
                profile,
            }))
        }
        TAG_FAILURE => {
            let length = read_u64(&mut from)? as usize;
            if length > 1024 {
                bail!("the decoder sent a {length}-byte message, which is more than a message");
            }
            let mut bytes = vec![0u8; length];
            from.read_exact(&mut bytes)?;
            Ok(Err(String::from_utf8_lossy(&bytes).into_owned()))
        }
        other => bail!("the decoder sent an unknown reply tag {other}"),
    }
}

/// The reply carries an image.
const TAG_IMAGE: u8 = 1;
/// The reply carries a reason the file would not decode.
const TAG_FAILURE: u8 = 2;

fn expect_header(mut from: impl Read) -> Result<()> {
    let mut magic = [0u8; 4];
    from.read_exact(&mut magic)?;
    if magic != MAGIC {
        bail!("the stream does not begin with the expected marker");
    }

    let mut version = [0u8; 2];
    from.read_exact(&mut version)?;
    let version = u16::from_le_bytes(version);
    if version != VERSION {
        bail!("the other end speaks protocol version {version}, this one speaks {VERSION}");
    }

    Ok(())
}

fn read_u32(mut from: impl Read) -> Result<u32> {
    let mut bytes = [0u8; 4];
    from.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(mut from: impl Read) -> Result<u64> {
    let mut bytes = [0u8; 8];
    from.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_survives_the_round_trip() {
        let mut wire = Vec::new();
        write_request(&mut wire, b"the file bytes").unwrap();
        assert_eq!(read_request(&wire[..]).unwrap(), b"the file bytes");
    }

    #[test]
    fn an_image_survives_the_round_trip() {
        let image = RawImage {
            width: 2,
            height: 1,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
            orientation: 1,
            profile: Vec::new(),
        };

        let mut wire = Vec::new();
        write_image(&mut wire, &image).unwrap();

        let Ok(Ok(decoded)) = read_reply(&wire[..]) else {
            panic!("the image did not survive the round trip");
        };
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.pixels, image.pixels);
    }

    #[test]
    fn a_failure_survives_the_round_trip() {
        let mut wire = Vec::new();
        write_failure(&mut wire, "the JPEG data is malformed").unwrap();
        let Ok(Err(message)) = read_reply(&wire[..]) else {
            panic!("the failure did not survive the round trip");
        };
        assert_eq!(message, "the JPEG data is malformed");
    }

    /// What the decoder learned about the file has to cross with the pixels.
    /// Reading it in the parent instead loses it for AVIF, whose colour
    /// description lives in the bitstream where only the decoder sees it.
    #[test]
    fn the_orientation_and_profile_cross_with_the_pixels() {
        let profile = moxcms::ColorProfile::new_srgb().encode().expect("encoding a profile");
        let image = RawImage {
            width: 2,
            height: 2,
            pixels: vec![7; 16],
            // A quarter turn, as EXIF numbers it.
            orientation: 6,
            profile: profile.clone(),
        };

        let mut wire = Vec::new();
        write_image(&mut wire, &image).unwrap();

        let Ok(Ok(decoded)) = read_reply(&wire[..]) else {
            panic!("the image did not survive the round trip");
        };
        assert_eq!(decoded.orientation, 6);
        assert_eq!(decoded.profile, profile);
        assert_eq!(decoded.pixels, vec![7; 16]);
    }

    /// An untagged image says so by sending no profile at all, which must not
    /// be confused with a profile of length zero being lost somewhere.
    #[test]
    fn an_untagged_image_crosses_without_a_profile() {
        let image = RawImage {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
            orientation: 1,
            profile: Vec::new(),
        };

        let mut wire = Vec::new();
        write_image(&mut wire, &image).unwrap();

        let Ok(Ok(decoded)) = read_reply(&wire[..]) else {
            panic!("the image did not survive the round trip");
        };
        assert!(decoded.profile.is_empty());
    }

    /// The profile arrives from the process that read the hostile file, so its
    /// length is checked like every other number it sends.
    #[test]
    fn an_absurd_profile_length_is_refused_rather_than_allocated_for() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&MAGIC);
        wire.extend_from_slice(&VERSION.to_le_bytes());
        wire.push(TAG_IMAGE);
        wire.extend_from_slice(&1u32.to_le_bytes());
        wire.extend_from_slice(&1u32.to_le_bytes());
        wire.push(1);
        // A profile larger than any real one, and larger than the cap.
        wire.extend_from_slice(&(u64::MAX / 2).to_le_bytes());

        assert!(read_reply(&wire[..]).is_err());
    }

    /// And the writer refuses to send one, so a decoder cannot push the
    /// viewer past the cap from its side either.
    #[test]
    fn writing_an_oversized_profile_is_refused() {
        let image = RawImage {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
            orientation: 1,
            profile: vec![0; MAX_PROFILE + 1],
        };

        assert!(write_image(Vec::new(), &image).is_err());
    }

    /// The decoder is the process holding the hostile file, so what it says
    /// on the way back is untrusted too: a claimed size that does not match
    /// the pixels sent must be refused, not allocated for.
    #[test]
    fn a_reply_whose_size_does_not_match_its_pixels_is_refused() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&MAGIC);
        wire.extend_from_slice(&VERSION.to_le_bytes());
        wire.push(TAG_IMAGE);
        wire.extend_from_slice(&1000u32.to_le_bytes());
        wire.extend_from_slice(&1000u32.to_le_bytes());
        // Four bytes of pixels for a four-million-byte image.
        wire.extend_from_slice(&4u64.to_le_bytes());
        wire.extend_from_slice(&[0, 0, 0, 0]);

        assert!(read_reply(&wire[..]).is_err());
    }

    /// Dimensions that would overflow the multiplication must not wrap into a
    /// small allocation that the pixels then overrun.
    #[test]
    fn absurd_dimensions_are_refused_rather_than_overflowing() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&MAGIC);
        wire.extend_from_slice(&VERSION.to_le_bytes());
        wire.push(TAG_IMAGE);
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        wire.extend_from_slice(&u32::MAX.to_le_bytes());
        wire.extend_from_slice(&16u64.to_le_bytes());
        wire.extend_from_slice(&[0; 16]);

        assert!(read_reply(&wire[..]).is_err());
    }

    #[test]
    fn a_stream_that_is_not_the_protocol_is_refused() {
        assert!(read_reply(&b"hello there"[..]).is_err());
        assert!(read_request(&b"hello there"[..]).is_err());
    }

    #[test]
    fn a_different_protocol_version_is_refused() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&MAGIC);
        wire.extend_from_slice(&(VERSION + 1).to_le_bytes());
        wire.push(TAG_IMAGE);
        assert!(read_reply(&wire[..]).is_err());
    }

    #[test]
    fn an_unknown_reply_tag_is_refused() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&MAGIC);
        wire.extend_from_slice(&VERSION.to_le_bytes());
        wire.push(99);
        assert!(read_reply(&wire[..]).is_err());
    }

    /// A truncated stream is a decoder that died mid-reply, which is exactly
    /// what the sandbox exists to survive.
    #[test]
    fn a_truncated_reply_is_an_error_rather_than_a_hang_or_a_panic() {
        let image = RawImage {
            width: 4,
            height: 4,
            pixels: vec![0; 64],
            orientation: 1,
            profile: Vec::new(),
        };
        let mut wire = Vec::new();
        write_image(&mut wire, &image).unwrap();

        for cut in 0..wire.len() {
            assert!(read_reply(&wire[..cut]).is_err(), "a reply cut at {cut} bytes was accepted");
        }
    }
}
