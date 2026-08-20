//! Decoding AVIF: the container, the AV1 frame inside it, and the colour it
//! declares.
//!
//! AVIF is an AV1 keyframe wrapped in the same ISOBMFF container HEIC uses.
//! The two halves are decoded by two crates: `avif-parse` walks the container
//! and hands over the payload of the primary item, and `rav1d` — dav1d
//! translated to Rust — turns that payload into planes.
//!
//! `rav1d` exposes dav1d's C interface rather than a Rust one, so the calls
//! below are `unsafe` and the safety argument for each is written where it is
//! made. That boundary is the whole reason this lives in its own module: the
//! rest of the viewer sees a function taking bytes and returning pixels.
//!
//! Unlike HEIC, nothing here resolves colour on the way in. The frame comes
//! back as YUV with the coefficients the file declares, this module converts
//! it to RGB with exactly those coefficients, and the primaries and transfer
//! curve are handed on as a profile for the shader to apply — which is how
//! every other format in the viewer is treated. See
//! `docs/adr/0008-avif-decodes-with-rav1d.md`.

use std::ptr::NonNull;

use anyhow::{Context, Result, bail};
use moxcms::{CicpProfile, ColorProfile};
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{dav1d_close, dav1d_data_create, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data};

use crate::image_source::{DecodedImage, Orientation};

/// The decoder wants more input before it can produce a picture.
///
/// dav1d reports this as `EAGAIN`, and for a still image it means the frame is
/// in flight rather than missing: asking again is what completes it.
const EAGAIN: i32 = -11;

/// How many times to ask again before giving up.
///
/// A still image needs one or two; a file that has not produced a picture
/// after this many is not going to.
const MAX_ATTEMPTS: usize = 16;

/// A decoded AVIF: pixels, and what the file says they mean.
pub struct Decoded {
    pub image: DecodedImage,
    /// The colour space the file declares, as a profile for the shader.
    ///
    /// `None` when the file says nothing about its colour, which — following
    /// ADR 0005 — is left untagged rather than assumed to be sRGB.
    pub profile: Option<ColorProfile>,
    /// How the container asks for the picture to be turned.
    ///
    /// `None` when it asks for nothing, and the EXIF tag — if there is one —
    /// decides instead.
    pub orientation: Option<Orientation>,
}

/// Decode an AVIF into RGBA8.
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    // `avif-parse` asserts its way through some malformed files rather than
    // returning an error — a damaged AVIF makes it panic inside its own box
    // reader. A picture arriving from the internet must not be able to take
    // the viewer down, so the parse is caught and reported as what it is: a
    // file that would not open. The decoder itself needs no such net; it
    // reports failures.
    let parsed = std::panic::catch_unwind(|| {
        let mut cursor = bytes;
        avif_parse::read_avif(&mut cursor).map_err(|error| format!("{error:?}"))
    });

    let container = match parsed {
        Ok(Ok(container)) => container,
        Ok(Err(error)) => bail!("reading the AVIF container: {error}"),
        Err(_) => bail!("the AVIF container is malformed"),
    };

    let frame = decode_av1(&container.primary_item).context("decoding the AV1 image")?;

    // The alpha channel is a second, monochrome AV1 image beside the first.
    // A file whose colour decodes but whose alpha does not is shown opaque
    // rather than refused: the picture is there, and half of it is better
    // than an error.
    let alpha = container.alpha_item.as_ref().and_then(|item| decode_av1(item).ok());

    let profile = profile_from(&frame);
    let image = to_rgba8(&frame, alpha.as_ref(), container.premultiplied_alpha)?;

    Ok(Decoded {
        image,
        profile,
        orientation: orientation_from(bytes),
    })
}

/// One decoded AV1 frame, copied out of the decoder's own buffers.
///
/// Copied rather than borrowed because the picture must be released back to
/// `rav1d` before this function returns; holding its planes past that would be
/// a use-after-free that the type system cannot see through the C interface.
struct Frame {
    width: u32,
    height: u32,
    /// One entry per plane: Y, U, V. Monochrome frames carry only Y.
    planes: Vec<Vec<u8>>,
    /// Row length of each plane in samples.
    strides: Vec<usize>,
    /// How chroma is sampled relative to luma, as horizontal and vertical
    /// shifts: 4:2:0 is `(1, 1)`, 4:2:2 is `(1, 0)`, 4:4:4 is `(0, 0)`.
    chroma_shift: (u32, u32),
    /// What the bitstream says its colour means, straight from the sequence
    /// header — CICP code points, not yet interpreted.
    primaries: u8,
    transfer: u8,
    matrix: u8,
    full_range: bool,
}

impl Frame {
    fn is_monochrome(&self) -> bool {
        self.planes.len() < 3
    }
}

/// Run one AV1 payload through `rav1d`.
fn decode_av1(payload: &[u8]) -> Result<Frame> {
    if payload.is_empty() {
        bail!("the image carries no AV1 data");
    }

    // SAFETY: every pointer handed to `rav1d` below is either a live local or
    // a buffer it allocated itself, and the context and picture are released
    // on every path out — including the error paths, which return through the
    // guards rather than past them.
    unsafe {
        let mut settings: Dav1dSettings = std::mem::zeroed();
        dav1d_default_settings(NonNull::from(&mut settings));

        let mut context: Option<Dav1dContext> = None;
        let result = dav1d_open(Some(NonNull::from(&mut context)), Some(NonNull::from(&mut settings)));
        if result.0 < 0 {
            bail!("the AV1 decoder would not start ({})", result.0);
        }
        let context = Context_(context);

        let mut data: Dav1dData = std::mem::zeroed();
        let buffer = dav1d_data_create(Some(NonNull::from(&mut data)), payload.len());
        if buffer.is_null() {
            bail!("the AV1 decoder would not take the image");
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), buffer, payload.len());

        let result = dav1d_send_data(context.0, Some(NonNull::from(&mut data)));
        if result.0 < 0 {
            bail!("the AV1 data was refused ({})", result.0);
        }

        let mut picture: Dav1dPicture = std::mem::zeroed();
        let mut result = dav1d_get_picture(context.0, Some(NonNull::from(&mut picture)));
        let mut attempts = 0;
        while result.0 == EAGAIN && attempts < MAX_ATTEMPTS {
            attempts += 1;
            result = dav1d_get_picture(context.0, Some(NonNull::from(&mut picture)));
        }
        if result.0 < 0 {
            bail!("the AV1 image would not decode ({})", result.0);
        }
        let picture = Picture(picture);

        copy_out(&picture.0)
    }
}

/// Copy a decoded picture out of the decoder's buffers.
///
/// # Safety
///
/// `picture` must be a picture `rav1d` has filled in and not yet released.
unsafe fn copy_out(picture: &Dav1dPicture) -> Result<Frame> {
    let width = picture.p.w as usize;
    let height = picture.p.h as usize;
    if width == 0 || height == 0 {
        bail!("the AV1 image has no size");
    }

    let sequence = picture.seq_hdr.context("the AV1 image carries no sequence header")?;
    // SAFETY: the header belongs to the picture, which is alive for this call.
    let sequence = unsafe { sequence.as_ref() };

    if picture.p.bpc != 8 {
        // 10- and 12-bit AVIF exist and are worth opening; they are not yet,
        // because the renderer uploads RGBA8 and narrowing here would throw
        // the depth away silently. Refused with a reason rather than shown
        // wrong. HDR (v0.12.0) is where the wider buffer arrives.
        bail!("a {}-bit AVIF is not one this build can show yet", picture.p.bpc);
    }

    let layout = sequence.layout;
    // dav1d's layout values: 0 is monochrome, 1 is 4:2:0, 2 is 4:2:2, 3 is
    // 4:4:4. The shifts say how much smaller the chroma planes are.
    let (chroma_shift, plane_count) = match layout {
        0 => ((0, 0), 1),
        1 => ((1, 1), 3),
        2 => ((1, 0), 3),
        3 => ((0, 0), 3),
        other => bail!("an AVIF with pixel layout {other} is not one this build can show"),
    };

    let mut planes = Vec::with_capacity(plane_count);
    let mut strides = Vec::with_capacity(plane_count);
    for index in 0..plane_count {
        let plane = picture.data[index].context("the AV1 image is missing a plane")?;
        // Chroma planes are shorter and narrower by the layout's shifts.
        let (rows, stride) = if index == 0 {
            (height, picture.stride[0] as usize)
        } else {
            (height.div_ceil(1 << chroma_shift.1), picture.stride[1] as usize)
        };

        let mut copied = vec![0u8; rows * stride];
        // SAFETY: the plane holds `rows` rows of `stride` bytes, which is what
        // `rav1d` allocated it for, and the destination is that size exactly.
        unsafe { std::ptr::copy_nonoverlapping(plane.as_ptr() as *const u8, copied.as_mut_ptr(), rows * stride) };
        planes.push(copied);
        strides.push(stride);
    }

    Ok(Frame {
        width: width as u32,
        height: height as u32,
        planes,
        strides,
        chroma_shift,
        primaries: sequence.pri as u8,
        transfer: sequence.trc as u8,
        matrix: sequence.mtrx as u8,
        full_range: sequence.color_range != 0,
    })
}

/// Read the rotation and mirroring the container asks for.
///
/// AVIF states these as `irot` and `imir` item properties, and — unlike HEIC,
/// whose decoder applies them on the way out — nothing here has applied them
/// by the time the pixels arrive. An encoder writing a rotated AVIF may write
/// nothing else: libavif records the turn as `irot` alone, with no EXIF item
/// anywhere in the file, so a viewer reading only EXIF shows such a picture on
/// its side.
///
/// `avif-parse` does not surface these properties, so the two boxes are found
/// here. They are fixed-size and trivially shaped — a length, a name, one byte
/// of payload — which is why this is a scan for the box rather than a second
/// container parser.
///
/// Returns `None` when the file asks for nothing, leaving the EXIF tag to
/// decide.
fn orientation_from(bytes: &[u8]) -> Option<Orientation> {
    // Both properties live inside `ipco`, the item property container. Bounding
    // the search to it keeps a run of bytes elsewhere in the file — inside the
    // compressed image data, say — from being read as a property.
    let properties = find_box(bytes, b"ipco")?;

    // `irot`: one byte, the low two bits counting quarter turns anticlockwise.
    let rotation = find_box(properties, b"irot")
        .and_then(|body| body.first())
        .map(|value| value & 0b11)
        .unwrap_or(0);
    // `imir`: one byte, the low bit choosing the axis. 0 mirrors top-to-bottom
    // (a vertical flip), 1 mirrors left-to-right.
    let mirror = find_box(properties, b"imir").and_then(|body| body.first()).map(|value| value & 0b1);

    if rotation == 0 && mirror.is_none() {
        return None;
    }

    // The EXIF vocabulary the renderer already speaks describes each of the
    // eight combinations, so the pair is mapped onto it rather than growing a
    // second way to say the same thing.
    Some(match (rotation, mirror) {
        (0, None) => Orientation::Normal,
        (1, None) => Orientation::Rotate270,
        (2, None) => Orientation::Rotate180,
        (3, None) => Orientation::Rotate90,
        // A mirrored image, then turned: the flip axis and the quarter turns
        // combine into the transposed and flipped orientations.
        (0, Some(0)) => Orientation::FlipVertical,
        (0, Some(1)) => Orientation::FlipHorizontal,
        (1, Some(0)) => Orientation::Transverse,
        (1, Some(1)) => Orientation::Transpose,
        (2, Some(0)) => Orientation::FlipHorizontal,
        (2, Some(1)) => Orientation::FlipVertical,
        (3, Some(0)) => Orientation::Transpose,
        (3, Some(1)) => Orientation::Transverse,
        // Unreachable: rotation is masked to two bits and mirror to one.
        _ => Orientation::Normal,
    })
}

/// Find a box by name anywhere in `bytes`, and return its body.
///
/// ISOBMFF boxes are length-prefixed, named by four bytes, and nested: the
/// rotation this module wants sits inside `ipco`, inside `iprp`, inside
/// `meta`. So the walk descends into every box it does not recognise rather
/// than stepping over the lot at one level.
///
/// The bytes come from a file that may be lying about them, so a length that
/// would not advance the walk — zero, or one running past the end — ends it
/// instead of looping or reading past the buffer. Recursion is bounded by
/// `depth` for the same reason: a file can nest boxes as deeply as it likes.
fn find_box<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    find_box_within(bytes, name, 0)
}

/// How deep the search will follow nested boxes.
///
/// The properties this module reads sit four levels down; twice that is room
/// for any real file and a bound against one built to recurse for ever.
const MAX_DEPTH: u32 = 8;

fn find_box_within<'a>(bytes: &'a [u8], name: &[u8; 4], depth: u32) -> Option<&'a [u8]> {
    if depth > MAX_DEPTH {
        return None;
    }

    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]) as usize;
        let kind = &bytes[offset + 4..offset + 8];

        // A box shorter than its own header, or longer than what is left, is
        // a file this cannot walk any further.
        let end = if length < 8 || offset + length > bytes.len() {
            bytes.len()
        } else {
            offset + length
        };

        let body = bytes.get(offset + 8..end)?;

        if kind == name {
            return Some(body);
        }

        // `meta` is a FullBox: four bytes of version and flags sit between its
        // header and the boxes inside it. Reading those four bytes as a length
        // would derail the walk one level down — which is where the properties
        // this module wants actually live.
        let body = if kind == b"meta" { body.get(4..).unwrap_or_default() } else { body };

        // Not this box: it may still hold the one being looked for.
        if let Some(found) = find_box_within(body, name, depth + 1) {
            return Some(found);
        }

        // A length that does not advance would leave the walk in place.
        if length < 8 || offset + length > bytes.len() {
            return None;
        }
        offset += length;
    }
    None
}

/// Turn the frame's colour description into a profile the shader can apply.
///
/// Returns `None` when the file describes nothing this build can place, so an
/// untagged AVIF passes through untouched — the same rule every other format
/// follows (ADR 0005).
///
/// The recognised code points are listed here rather than inferred from the
/// conversion: `moxcms` maps every value outside the standard onto `Reserved`
/// instead of refusing it, returns `false` even when it has applied a colour
/// space successfully, and records `cicp` either way. None of those three
/// answers distinguishes "converted" from "left alone", so this decides for
/// itself what it is willing to claim.
fn profile_from(frame: &Frame) -> Option<ColorProfile> {
    if !describes_a_colour_space(frame.primaries, frame.transfer) {
        return None;
    }

    let cicp = CicpProfile {
        color_primaries: frame.primaries.try_into().ok()?,
        transfer_characteristics: frame.transfer.try_into().ok()?,
        matrix_coefficients: frame.matrix.try_into().ok()?,
        full_range: frame.full_range,
    };

    // Built on top of sRGB so the tags a profile needs are all present; the
    // primaries and the tone curve are then replaced by what the file states.
    let mut profile = ColorProfile::new_srgb();
    profile.update_rgb_colorimetry_from_cicp(cicp);
    Some(profile)
}

/// Whether a pair of CICP code points names a colour space with real
/// chromaticity and a real tone curve.
///
/// Code point 2 is "unspecified" in both lists, and everything reserved or
/// beyond the standard is a file describing something this build cannot place.
fn describes_a_colour_space(primaries: u8, transfer: u8) -> bool {
    // ITU-T H.273 Table 2: the primaries with defined chromaticity.
    const PRIMARIES: [u8; 11] = [1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 22];
    // Table 3: the transfer characteristics with a defined curve.
    const TRANSFER: [u8; 16] = [1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18];

    PRIMARIES.contains(&primaries) && TRANSFER.contains(&transfer)
}

/// Convert the decoded planes to the RGBA8 the renderer uploads.
fn to_rgba8(frame: &Frame, alpha: Option<&Frame>, premultiplied: bool) -> Result<DecodedImage> {
    let width = frame.width as usize;
    let height = frame.height as usize;
    let expected = crate::image_source::pixel_count(frame.width, frame.height)?;

    let (kr, kb) = luma_coefficients(frame.matrix);
    let mut pixels = vec![0u8; expected];

    for y in 0..height {
        for x in 0..width {
            let luma = frame.planes[0][y * frame.strides[0] + x];

            let (red, green, blue) = if frame.is_monochrome() {
                // A monochrome AVIF has no chroma to combine: the luma sample
                // is the colour, spread across the three channels.
                let grey = expand_range(luma, frame.full_range);
                (grey, grey, grey)
            } else {
                let chroma_x = x >> frame.chroma_shift.0;
                let chroma_y = y >> frame.chroma_shift.1;
                let cb = frame.planes[1][chroma_y * frame.strides[1] + chroma_x];
                let cr = frame.planes[2][chroma_y * frame.strides[2] + chroma_x];
                ycbcr_to_rgb(luma, cb, cr, kr, kb, frame.full_range)
            };

            let opacity = match alpha {
                // The alpha image is a monochrome frame of the same size, so
                // its luma plane is the coverage.
                Some(alpha) if !alpha.planes.is_empty() => {
                    let row = y.min(alpha.height.saturating_sub(1) as usize);
                    let column = x.min(alpha.width.saturating_sub(1) as usize);
                    expand_range(alpha.planes[0][row * alpha.strides[0] + column], alpha.full_range)
                }
                _ => 0xFF,
            };

            let index = (y * width + x) * 4;
            // A premultiplied file stores colour already scaled by coverage;
            // the renderer blends as though it were not, so it is undone here.
            let (red, green, blue) = if premultiplied && opacity != 0 {
                (unpremultiply(red, opacity), unpremultiply(green, opacity), unpremultiply(blue, opacity))
            } else {
                (red, green, blue)
            };

            pixels[index] = red;
            pixels[index + 1] = green;
            pixels[index + 2] = blue;
            pixels[index + 3] = opacity;
        }
    }

    Ok(DecodedImage {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

/// The luma weights for a set of matrix coefficients, as CICP numbers them.
///
/// Anything unrecognised falls back to BT.709, which is what an AVIF without
/// a stated matrix is in practice.
fn luma_coefficients(matrix: u8) -> (f32, f32) {
    match matrix {
        // BT.601 / SMPTE 170M — what most still images declare.
        5 | 6 => (0.299, 0.114),
        // BT.2020 non-constant luminance.
        9 => (0.2627, 0.0593),
        // BT.709 and everything unrecognised.
        _ => (0.2126, 0.0722),
    }
}

/// Convert one YCbCr sample to RGB.
fn ycbcr_to_rgb(luma: u8, cb: u8, cr: u8, kr: f32, kb: f32, full_range: bool) -> (u8, u8, u8) {
    let (y, cb, cr) = if full_range {
        (luma as f32, cb as f32 - 128.0, cr as f32 - 128.0)
    } else {
        // Limited range packs luma into 16..235 and chroma into 16..240.
        (
            ((luma as f32 - 16.0) * 255.0 / 219.0),
            (cb as f32 - 128.0) * 255.0 / 224.0,
            (cr as f32 - 128.0) * 255.0 / 224.0,
        )
    };

    let kg = 1.0 - kr - kb;
    let red = y + 2.0 * (1.0 - kr) * cr;
    let blue = y + 2.0 * (1.0 - kb) * cb;
    let green = y - (2.0 * (1.0 - kr) * kr / kg) * cr - (2.0 * (1.0 - kb) * kb / kg) * cb;

    (clamp(red), clamp(green), clamp(blue))
}

/// Stretch a limited-range sample to the full 0..255 the renderer expects.
fn expand_range(sample: u8, full_range: bool) -> u8 {
    if full_range { sample } else { clamp((sample as f32 - 16.0) * 255.0 / 219.0) }
}

/// Undo premultiplication for one channel.
fn unpremultiply(channel: u8, opacity: u8) -> u8 {
    clamp(channel as f32 * 255.0 / opacity as f32)
}

fn clamp(value: f32) -> u8 {
    value.clamp(0.0, 255.0) as u8
}

/// A decoder context closed when it goes out of scope.
///
/// Named with a trailing underscore because `Context` is `anyhow`'s trait,
/// which this module also uses.
struct Context_(Option<Dav1dContext>);

impl Drop for Context_ {
    fn drop(&mut self) {
        // SAFETY: the context came from `dav1d_open` and is closed once.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.0))) };
    }
}

/// A picture released back to the decoder when it goes out of scope.
struct Picture(Dav1dPicture);

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: the picture came from `dav1d_get_picture` and is released
        // once.
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.0))) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16x16 gradient written by libavif, an implementation independent of
    /// the decoder under test — the same arrangement JPEG XL and HEIC use.
    ///
    /// Carried as text because the suite ships no binary fixtures, and small
    /// because an AV1 payload cannot be assembled from a few bytes of header.
    const AVIF_GRADIENT: &str = concat!(
        "AAAAIGZ0eXBhdmlmAAAAAGF2aWZtaWYxbWlhZk1BMUIAAADrbWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAcGljdAAAAAAAAAAA",
        "AAAAAAAAAAAOcGl0bQAAAAAAAQAAAB5pbG9jAAAAAEQAAAEAAQAAAAEAAAETAAAAhAAAAChpaW5mAAAAAAABAAAAGmluZmUC",
        "AAAAAAEAAGF2MDFDb2xvcgAAAABqaXBycAAAAEtpcGNvAAAAFGlzcGUAAAAAAAAAEAAAABAAAAAQcGl4aQAAAAADCAgIAAAA",
        "DGF2MUOBAAwAAAAAE2NvbHJuY2x4AAEADQAGgAAAABdpcG1hAAAAAAAAAAEAAQQBAoMEAAAAjG1kYXQSAAoJGAz/2iAhoNCA",
        "MnURAapAAHkAANDFB71kq9efT2sWjvRlzns6LGzW3tUE2GewfYbH33rLzg7/xLcaV/GQup2AmQG5I6Ttsd61Vr9V7LZVD9Bp",
        "5CTdjoF8eGOmJmYWtDDRUraz3k9gaK+qHWq0XXIQUGe3Ts4AmqVq20ZLShuhgdg=",
    );

    /// The same size, half-transparent red, so the alpha auxiliary image is
    /// exercised rather than assumed.
    const AVIF_TRANSLUCENT: &str = concat!(
        "AAAAIGZ0eXBhdmlmAAAAAGF2aWZtaWYxbWlhZk1BMUIAAAGGbWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAcGljdAAAAAAAAAAA",
        "AAAAAAAAAAAOcGl0bQAAAAAAAQAAACxpbG9jAAAAAEQAAAIAAQAAAAEAAAHAAAAALwACAAAAAQAAAa4AAAASAAAAQmlpbmYA",
        "AAAAAAIAAAAaaW5mZQIAAAAAAQAAYXYwMUNvbG9yAAAAABppbmZlAgAAAAACAABhdjAxQWxwaGEAAAAAGmlyZWYAAAAAAAAA",
        "DmF1eGwAAgABAAEAAADDaXBycAAAAJ1pcGNvAAAAFGlzcGUAAAAAAAAAEAAAABAAAAAQcGl4aQAAAAADCAgIAAAADGF2MUOB",
        "AAwAAAAAE2NvbHJuY2x4AAEADQAGgAAAAA5waXhpAAAAAAEIAAAADGF2MUOBABwAAAAAOGF1eEMAAAAAdXJuOm1wZWc6bXBl",
        "Z0I6Y2ljcDpzeXN0ZW1zOmF1eGlsaWFyeTphbHBoYQAAAAAeaXBtYQAAAAAAAAACAAEEAQKDBAACBAEFhgcAAABJbWRhdBIA",
        "CgUYDP/YVDIHEMAAAQAUgBIACgkYDP/aICGg0IAyIBEBqkAAeQAA0MUMvBjYhDjey1YmR1gY+xRzXtDBiH0c",
    );

    fn from_base64(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

        let mut out = Vec::new();
        let mut accumulator: u32 = 0;
        let mut bits = 0;
        for byte in text.bytes().filter(|byte| *byte != b'=') {
            let value = ALPHABET.iter().position(|candidate| *candidate == byte).expect("a base64 character") as u32;
            accumulator = (accumulator << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((accumulator >> bits) as u8);
            }
        }
        out
    }

    fn pixel_at(image: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let start = ((y * image.width + x) * 4) as usize;
        [image.pixels[start], image.pixels[start + 1], image.pixels[start + 2], image.pixels[start + 3]]
    }

    /// The format opens end to end: container, AV1 frame, colour conversion.
    #[test]
    fn decodes_an_avif_to_rgba() {
        let decoded = decode(&from_base64(AVIF_GRADIENT)).unwrap();
        assert_eq!((decoded.image.width, decoded.image.height), (16, 16));
        assert_eq!(decoded.image.pixels.len(), 16 * 16 * 4);
        assert!(
            decoded.image.pixels.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0xFF),
            "an opaque AVIF decoded with transparency"
        );

        // The gradient rises in red to the right and in green downwards.
        // Compared with a wide margin: AV1 stores colour at half resolution
        // and the fixture is lossy, so a corner is not the value encoded.
        let top_left = pixel_at(&decoded.image, 0, 0);
        let top_right = pixel_at(&decoded.image, 15, 0);
        let bottom_left = pixel_at(&decoded.image, 0, 15);
        assert!(
            top_right[0] > top_left[0] + 100,
            "red did not rise across the image: {top_left:?} to {top_right:?}"
        );
        assert!(
            bottom_left[1] > top_left[1] + 100,
            "green did not rise down the image: {top_left:?} to {bottom_left:?}"
        );
    }

    /// Checked against what libavif itself decodes the same file to, so the
    /// YUV conversion here is held to another implementation rather than to
    /// its own arithmetic.
    #[test]
    fn the_colour_matches_what_the_encoder_meant() {
        let decoded = decode(&from_base64(AVIF_GRADIENT)).unwrap();

        // libavif reads this fixture as these values at these corners.
        for (x, y, expected) in [(0u32, 0u32, [0u8, 1, 120]), (15, 0, [245, 4, 124]), (0, 15, [9, 252, 130])] {
            let actual = pixel_at(&decoded.image, x, y);
            for channel in 0..3 {
                let difference = actual[channel].abs_diff(expected[channel]);
                assert!(difference <= 8, "at ({x},{y}) channel {channel}: {actual:?} against libavif's {expected:?}");
            }
        }
    }

    /// An AVIF states its colour space in the bitstream, and it must reach the
    /// viewer as a profile: unlike HEIC, nothing here converts on the way in,
    /// so a file left untagged would be shown in the wrong colours.
    #[test]
    fn a_tagged_avif_carries_its_colour_space_out() {
        let decoded = decode(&from_base64(AVIF_GRADIENT)).unwrap();
        assert!(decoded.profile.is_some(), "the file declares its colour space and the profile was dropped");
    }

    /// Transparency survives: the alpha auxiliary is a second AV1 image, and
    /// missing it would show a translucent picture as opaque.
    #[test]
    fn an_avif_keeps_its_transparency() {
        let decoded = decode(&from_base64(AVIF_TRANSLUCENT)).unwrap();
        assert_eq!((decoded.image.width, decoded.image.height), (16, 16));

        let pixel = pixel_at(&decoded.image, 8, 8);
        assert!(pixel[3] > 100 && pixel[3] < 160, "the alpha channel was not read: {pixel:?}");
    }

    /// A 64x48 gradient libavif was asked to save rotated. It recorded the
    /// turn as an `irot` property and wrote no EXIF item at all — which is
    /// exactly the file a viewer reading only EXIF shows on its side.
    const AVIF_ROTATED: &str = concat!(
        "AAAAIGZ0eXBhdmlmAAAAAGF2aWZtaWYxbWlhZk1BMUIAAAD1bWV0YQAAAAAAAAAhaGRscgAAAAAAAAAAcGljdAAAAAAAAAAA",
        "AAAAAAAAAAAOcGl0bQAAAAAAAQAAAB5pbG9jAAAAAEQAAAEAAQAAAAEAAAEdAAABSwAAAChpaW5mAAAAAAABAAAAGmluZmUC",
        "AAAAAAEAAGF2MDFDb2xvcgAAAAB0aXBycAAAAFRpcGNvAAAAFGlzcGUAAAAAAAAAQAAAADAAAAAQcGl4aQAAAAADCAgIAAAA",
        "DGF2MUOBAAwAAAAAE2NvbHJuY2x4AAEADQAGgAAAAAlpcm90AwAAABhpcG1hAAAAAAAAAAEAAQUBAoMEhQAAAVNtZGF0EgAK",
        "CRgVf72iAhoNCDK7AhEBqkAAeQACz8o/e9MT3/rc7RbHI+Q8YJH7VtV0+sJWIuWDQ9xHpgnF2DKBJ8G/QwSCxCry/ytxCDLm",
        "+eeifp6vZSR4xLNvoARjF3FhhLjCKmYzzqTczgeSbAFrKfWze6Z0bhoZPb5BiepBS+kMiFA8HE3qmd/DEwbJTUk4DQ3WDrfh",
        "xxciossjUorAKxxMq/gzPJgWOIb9j8ZoXafpa8skQlqNnntOj1INPmGW8UI3FIy+MYqE8ThsQAyzcNs6RhD0UtkOGtLaeviU",
        "IppAEOAj2XSHK5q+LCUi/pxb2Zny48ovyGePH7kKzk8js4f/+8d8j5/058iKbnyMTqPx0JQSkaIV9cah+5l+j2U4T+tNrh2A",
        "85nRBlALowIdNcnMYanxDfuSqChVZ2yPLrs/otkYQuq7zbedVzZYtA==",
    );

    /// The rotation a real encoder writes lives in the container, not in EXIF.
    /// Missing it leaves a portrait photograph lying on its side.
    #[test]
    fn the_container_rotation_is_read() {
        let decoded = decode(&from_base64(AVIF_ROTATED)).unwrap();
        assert_eq!(
            decoded.orientation,
            Some(Orientation::Rotate90),
            "the irot property was not read, so a rotated AVIF would be shown unturned"
        );

        // The pixels themselves arrive as coded: 64x48, to be turned by the
        // renderer rather than rewritten here.
        assert_eq!((decoded.image.width, decoded.image.height), (64, 48));
    }

    /// A file that asks for nothing must say so, leaving the EXIF tag — if
    /// there is one — to decide.
    #[test]
    fn a_file_without_transform_properties_asks_for_nothing() {
        assert_eq!(decode(&from_base64(AVIF_GRADIENT)).unwrap().orientation, None);
    }

    /// The eight combinations of quarter turns and mirroring map onto the
    /// EXIF vocabulary the renderer already speaks. Getting one wrong turns a
    /// picture the wrong way, which no amount of decoding correctly fixes.
    #[test]
    fn every_rotation_and_mirror_maps_to_an_orientation() {
        // Quarter turns anticlockwise, as `irot` counts them. Zero turns is
        // a property asking for nothing, which reads the same as absent.
        assert_eq!(orientation_from(&ipco(&[irot(0)])), None);
        assert_eq!(orientation_from(&ipco(&[irot(1)])), Some(Orientation::Rotate270));
        assert_eq!(orientation_from(&ipco(&[irot(2)])), Some(Orientation::Rotate180));
        assert_eq!(orientation_from(&ipco(&[irot(3)])), Some(Orientation::Rotate90));

        // Mirroring alone: axis 0 is top-to-bottom, axis 1 left-to-right.
        assert_eq!(orientation_from(&ipco(&[imir(0)])), Some(Orientation::FlipVertical));
        assert_eq!(orientation_from(&ipco(&[imir(1)])), Some(Orientation::FlipHorizontal));

        // Both together.
        assert_eq!(orientation_from(&ipco(&[irot(1), imir(1)])), Some(Orientation::Transpose));
        assert_eq!(orientation_from(&ipco(&[irot(3), imir(0)])), Some(Orientation::Transpose));

        // Nothing stated at all.
        assert_eq!(orientation_from(&ipco(&[])), None);
    }

    /// The box walk runs over bytes from a file that may be lying about them.
    /// A length of zero, or one past the end, must end the walk rather than
    /// loop for ever or read past the buffer.
    #[test]
    fn a_lying_box_length_does_not_hang_or_overrun() {
        // A box claiming zero length, which would leave the walk in place.
        let mut zero = ipco(&[irot(1)]);
        zero[8..12].copy_from_slice(&0u32.to_be_bytes());
        let _ = orientation_from(&zero);

        // A box claiming to run far past the file.
        let mut huge = ipco(&[irot(1)]);
        huge[8..12].copy_from_slice(&9999u32.to_be_bytes());
        let _ = orientation_from(&huge);

        // And every truncation of a real file.
        let whole = from_base64(AVIF_ROTATED);
        for cut in (0..whole.len()).step_by(7) {
            let _ = orientation_from(&whole[..cut]);
        }
    }

    /// An `irot` box: length, name, one byte of payload.
    fn irot(quarter_turns: u8) -> Vec<u8> {
        let mut out = 9u32.to_be_bytes().to_vec();
        out.extend_from_slice(b"irot");
        out.push(quarter_turns);
        out
    }

    /// An `imir` box, shaped the same way.
    fn imir(axis: u8) -> Vec<u8> {
        let mut out = 9u32.to_be_bytes().to_vec();
        out.extend_from_slice(b"imir");
        out.push(axis);
        out
    }

    /// Wrap properties in the `ipco` container they live in, inside enough of
    /// an outer box for the walk to have something to descend through.
    fn ipco(properties: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = properties.concat();

        let mut ipco = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        ipco.extend_from_slice(b"ipco");
        ipco.extend_from_slice(&body);

        let mut iprp = ((ipco.len() + 8) as u32).to_be_bytes().to_vec();
        iprp.extend_from_slice(b"iprp");
        iprp.extend_from_slice(&ipco);
        iprp
    }

    /// The decoder reads hostile input by definition. A broken file must come
    /// back as an error, and every `unsafe` block above must still release
    /// what it took.
    #[test]
    fn a_broken_avif_is_an_error_rather_than_a_panic() {
        let whole = from_base64(AVIF_GRADIENT);
        for cut in [0, 1, 12, 40, whole.len() / 2, whole.len() - 1] {
            assert!(decode(&whole[..cut]).is_err(), "an AVIF cut to {cut} bytes decoded anyway");
        }

        let mut damaged = whole.clone();
        for index in (0..damaged.len()).step_by(31) {
            damaged[index] ^= 0xA5;
        }
        let _ = decode(&damaged);
    }

    /// Decoding the same file repeatedly must not leak a decoder context or a
    /// picture — the two things the guards in `decode_av1` exist to release.
    #[test]
    fn decoding_repeatedly_releases_what_it_takes() {
        let bytes = from_base64(AVIF_GRADIENT);
        for _ in 0..32 {
            decode(&bytes).expect("decoding");
        }
    }

    /// An unrecognised CICP code point means the file said nothing this build
    /// can place, and an image must then pass through untagged rather than be
    /// converted against a guess (ADR 0005).
    #[test]
    fn an_unrecognised_colour_description_leaves_the_image_untagged() {
        let mut frame = Frame {
            width: 1,
            height: 1,
            planes: vec![vec![0]],
            strides: vec![1],
            chroma_shift: (0, 0),
            primaries: 2,
            transfer: 2,
            matrix: 2,
            full_range: true,
        };
        assert!(profile_from(&frame).is_none(), "an unspecified colour description produced a profile");

        // A code point outside the standard entirely.
        frame.primaries = 200;
        frame.transfer = 200;
        assert!(profile_from(&frame).is_none(), "an unknown colour description produced a profile");

        // Reserved values sit between the two: within range, but naming
        // nothing.
        frame.primaries = 3;
        frame.transfer = 3;
        assert!(profile_from(&frame).is_none(), "a reserved colour description produced a profile");

        // And a real one still does produce a profile — otherwise this test
        // would pass with a function that always answers `None`.
        frame.primaries = 12;
        frame.transfer = 13;
        assert!(profile_from(&frame).is_some(), "Display P3 with an sRGB curve produced no profile");
    }

    /// The list of code points this build will convert from. Written out
    /// rather than taken from `moxcms`, which maps everything outside the
    /// standard onto `Reserved` instead of refusing it.
    #[test]
    fn only_real_colour_spaces_are_claimed() {
        // Unspecified, reserved, and past the end of the tables.
        for primaries in [0u8, 2, 3, 13, 21, 23, 200, 255] {
            assert!(!describes_a_colour_space(primaries, 13), "primaries {primaries} were claimed");
        }
        for transfer in [0u8, 2, 3, 19, 200, 255] {
            assert!(!describes_a_colour_space(1, transfer), "transfer {transfer} was claimed");
        }

        // The ones that name something real.
        assert!(describes_a_colour_space(1, 1), "BT.709 was not claimed");
        assert!(describes_a_colour_space(12, 13), "Display P3 with an sRGB curve was not claimed");
        assert!(describes_a_colour_space(9, 16), "BT.2020 with the PQ curve was not claimed");
    }

    /// The luma weights decide the whole conversion; naming the wrong ones
    /// shifts every colour in the image.
    #[test]
    fn the_luma_weights_follow_the_stated_matrix() {
        // BT.601, which most still images declare.
        assert_eq!(luma_coefficients(6), (0.299, 0.114));
        assert_eq!(luma_coefficients(5), (0.299, 0.114));
        // BT.709.
        assert_eq!(luma_coefficients(1), (0.2126, 0.0722));
        // BT.2020 non-constant luminance.
        assert_eq!(luma_coefficients(9), (0.2627, 0.0593));
        // Anything unrecognised falls back to BT.709 rather than to nothing.
        assert_eq!(luma_coefficients(200), (0.2126, 0.0722));
    }

    /// Grey must stay grey through the conversion, in both ranges: a mistake
    /// in the range handling shows up as a colour cast across the whole image.
    #[test]
    fn neutral_grey_survives_the_conversion() {
        let (kr, kb) = luma_coefficients(1);

        let (r, g, b) = ycbcr_to_rgb(128, 128, 128, kr, kb, true);
        assert_eq!((r, g, b), (128, 128, 128));

        // Limited range: 126 is mid-grey once 16..235 is stretched out.
        let (r, g, b) = ycbcr_to_rgb(126, 128, 128, kr, kb, false);
        for channel in [r, g, b] {
            assert!(channel.abs_diff(128) <= 2, "limited-range grey came out as {r},{g},{b}");
        }
    }
}
