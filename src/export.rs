//! Writing a picture back out as another file.
//!
//! What leaves here is what was on screen, not what was in the file. That is
//! the whole promise of the feature: a wide-gamut photograph handed to someone
//! else arrives in the colour its owner was looking at, rather than in numbers
//! their program will read some other way.
//!
//! So the colour path is the shader's, step for step — the same curves, the
//! same matrix, the same clamp — run on the CPU over every pixel instead of on
//! the GPU over the visible ones. `holds_the_same_colour_as_the_shader` in the
//! tests below holds the two together by reading the WGSL itself; a change to
//! one that is not made to the other fails the gate rather than quietly
//! shipping a file that does not match its preview.
//!
//! HDR is included in that and is the reason to say it out loud: an HDR source
//! exported to one of these formats gets the same treatment it gets on a
//! standard-range monitor. Reference white lands on white and everything above
//! it clips. Highlights that the screen could not show do not survive the trip
//! — no more and no less than what the owner saw.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::color::ColorTransform;
use crate::image_source::{DecodedImage, Depth};

/// A format a picture can be written as.
///
/// Deliberately short. These three are what a picture is sent as — a photograph
/// to someone's phone, a screenshot into a chat, a graphic with transparency
/// onto a page — and all three encode with what the viewer already carries for
/// decoding them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Jpeg,
    Png,
    WebP,
}

impl Target {
    /// Every target, in the order they appear above.
    pub const ALL: &'static [Target] = &[Target::Jpeg, Target::Png, Target::WebP];

    /// The extension a file of this kind is given.
    pub fn extension(self) -> &'static str {
        match self {
            Target::Jpeg => "jpg",
            Target::Png => "png",
            Target::WebP => "webp",
        }
    }

    /// What this is called in the interface.
    pub fn label(self) -> &'static str {
        match self {
            Target::Jpeg => "JPEG",
            Target::Png => "PNG",
            Target::WebP => "WebP",
        }
    }

    /// Whether a pixel's transparency survives being written as this.
    ///
    /// JPEG has no alpha channel at all, so a transparent picture saved as one
    /// arrives composited — which the interface says before it happens rather
    /// than after.
    pub fn keeps_alpha(self) -> bool {
        match self {
            Target::Jpeg => false,
            Target::Png | Target::WebP => true,
        }
    }

    /// The target matching an extension, for a name the user typed.
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some(Target::Jpeg),
            "png" => Some(Target::Png),
            "webp" => Some(Target::WebP),
            _ => None,
        }
    }
}

/// How the colour is carried across.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Colour {
    /// Write the file's own numbers, and hand its profile along with them.
    ///
    /// Nothing is converted, so nothing is lost; the receiving program is
    /// trusted to read the profile. The default, because it is the only one of
    /// the two that can be undone.
    #[default]
    KeepProfile,
    /// Convert to sRGB and write that.
    ///
    /// For a picture going somewhere that will ignore a profile — a chat, a
    /// forum, a printer's web form. The colour the owner saw is baked into the
    /// numbers, so a program that assumes sRGB is right for once.
    BakeToSrgb,
}

/// What a save was asked to do.
pub struct Request<'a> {
    pub image: &'a DecodedImage,
    /// The image's own profile, as read from the file. `None` for an untagged
    /// file, which states nothing about its colour (ADR 0005).
    pub profile: Option<&'a moxcms::ColorProfile>,
    pub target: Target,
    pub colour: Colour,
    /// JPEG and WebP quality, 1-100. Ignored by PNG, which is lossless.
    pub quality: u8,
    /// What the file states about its colour that the decoder did not honour,
    /// carried through so a save can repeat it.
    ///
    /// It matters more here than on screen: a picture whose colour is a little
    /// off is something a person can live with until they notice, and the same
    /// colour baked into a file sent to somebody else is not.
    pub caveat: Option<&'a str>,
}

/// What the picture will lose on the way out, in the words the interface says
/// before the file is written.
///
/// A warning is not a refusal: every one of these is something a person may
/// want anyway, and the point is that they know rather than find out later.
pub fn warnings(request: &Request<'_>) -> Vec<String> {
    let mut said = Vec::new();

    if is_hdr(request.profile) {
        said.push("HDR to SDR, as on an ordinary screen: highlights above white are clipped".to_string());
    }

    // What the viewer could not honour when it read the file, repeated at the
    // moment it would be written into a new one. The colour passport, on `K`,
    // says the same thing at more length.
    if let Some(caveat) = request.caveat {
        said.push(format!("{caveat} See the colour passport (K)."));
    }

    if !request.target.keeps_alpha() && has_transparency(request.image) {
        said.push(format!("{} has no transparency: it will be filled with white", request.target.label()));
    }

    if request.image.depth == Depth::Sixteen && request.target != Target::Png {
        said.push(format!("{} stores 8 bits per channel: the extra precision is dropped", request.target.label()));
    }

    said
}

/// Whether this profile describes an HDR image.
///
/// PQ is the one transfer function here that is absolute rather than relative:
/// its white is a number of nits, not "as bright as the screen goes". That is
/// what makes an export from it a conversion rather than a copy.
pub fn is_hdr(profile: Option<&moxcms::ColorProfile>) -> bool {
    profile
        .and_then(|profile| profile.cicp)
        .is_some_and(|cicp| cicp.transfer_characteristics == moxcms::TransferCharacteristics::Smpte2084)
}

/// Whether any pixel is less than fully opaque.
fn has_transparency(image: &DecodedImage) -> bool {
    match image.depth {
        Depth::Eight => image.pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] != 0xFF),
        Depth::Sixteen => image.pixels.as_chunks::<8>().0.iter().any(|pixel| pixel[6..8] != [0xFF, 0xFF]),
    }
}

/// Write the picture to `path`.
pub fn save(request: &Request<'_>, path: &Path) -> Result<()> {
    let bytes = encode(request)?;
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Encode the picture, without touching the disk.
///
/// Separate from `save` because the clipboard wants the same bytes, and
/// because a test can hold these to account without a temporary file.
pub fn encode(request: &Request<'_>) -> Result<Vec<u8>> {
    if request.image.width == 0 || request.image.height == 0 {
        bail!("there is no picture to save");
    }

    let pixels = to_eight_bit(request);
    let width = request.image.width;
    let height = request.image.height;

    let mut out = Vec::new();
    match request.target {
        Target::Jpeg => {
            // JPEG carries no alpha, so the picture is composited onto white
            // first — the warning above says so before this runs.
            let opaque = over_white(&pixels);
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), request.quality.clamp(1, 100));
            encoder
                .encode(&opaque, width, height, image::ExtendedColorType::Rgb8)
                .context("encoding a JPEG")?;
        }
        Target::Png => {
            let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut out));
            image::ImageEncoder::write_image(encoder, &pixels, width, height, image::ExtendedColorType::Rgba8).context("encoding a PNG")?;
        }
        Target::WebP => {
            let encoder = image_webp::WebPEncoder::new(Cursor::new(&mut out));
            encoder
                .encode(&pixels, width, height, image_webp::ColorType::Rgba8)
                .context("encoding a WebP")?;
        }
    }

    Ok(out)
}

/// Take a rectangle out of a picture, in the coordinates it is *shown* in.
///
/// Shown, not stored, and that is the whole reason this is not four lines of
/// slicing: a photograph from a phone held sideways carries an orientation, so
/// the rectangle a person dragged over their screen is not the rectangle of
/// the same numbers in the file. The mapping is `eyedropper::sample`, which is
/// the one place the viewer turns a shown pixel into a stored one — a second
/// copy of that table here is exactly the drift ADR 0018 warns about.
///
/// The result is upright: the orientation has been applied by the reading, so
/// the file that is written needs no tag to look right.
pub fn cut(image: &DecodedImage, orientation: crate::image_source::Orientation, rect: (u32, u32, u32, u32)) -> Result<DecodedImage> {
    let (x, y, width, height) = rect;
    if width == 0 || height == 0 {
        bail!("there is nothing to crop to");
    }

    let shown = crate::eyedropper::shown_size(image, orientation);
    if x.saturating_add(width) > shown.0 || y.saturating_add(height) > shown.1 {
        bail!("the crop reaches outside the picture");
    }

    // Eight bits out, whatever came in. The re-encoding path writes PNG, and a
    // sixteen-bit source narrowed here loses precision the warning in the save
    // box already names for every other export.
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for row in 0..height {
        for column in 0..width {
            match crate::eyedropper::sample(image, orientation, shown, (x + column, y + row)) {
                Some((raw, alpha)) => {
                    let narrow = |channel: u16| match image.depth {
                        Depth::Eight => channel as u8,
                        Depth::Sixteen => (channel >> 8) as u8,
                    };
                    pixels.extend_from_slice(&[narrow(raw[0]), narrow(raw[1]), narrow(raw[2]), alpha]);
                }
                // Inside the bounds checked above, so this cannot happen; a
                // transparent pixel rather than a panic if it ever does, since
                // the length of the buffer is what says how big the crop is.
                None => pixels.extend_from_slice(&[0, 0, 0, 0]),
            }
        }
    }

    Ok(DecodedImage {
        width,
        height,
        pixels,
        depth: Depth::Eight,
    })
}

/// Shrink a picture so its longest side is at most `width`, by averaging.
///
/// Averaging rather than picking nearest pixels: a photograph reduced by
/// dropping pixels aliases into a mess of stair-steps, and the whole reason to
/// send a smaller copy is that it still looks like the picture.
///
/// A picture already inside the bound is returned untouched — resizing it up
/// would be inventing detail nobody asked for.
pub fn shrink(image: &DecodedImage, width: u32) -> DecodedImage {
    let longest = image.width.max(image.height);
    if width == 0 || longest <= width {
        return image.clone();
    }

    let scale = width as f64 / longest as f64;
    let out_width = ((image.width as f64 * scale).round() as u32).max(1);
    let out_height = ((image.height as f64 * scale).round() as u32).max(1);

    // Eight bits: the copy is going into a chat window, and the box's own
    // warning already says a sixteen-bit source loses its extra precision on
    // the way to a JPEG.
    let source = narrow(image);
    let mut out = Vec::with_capacity((out_width * out_height * 4) as usize);

    for row in 0..out_height {
        // The box of source pixels this one is the average of. Taken from the
        // edges rather than from a centre and a radius, so every source pixel
        // belongs to exactly one box and none is counted twice or skipped.
        let top = (row as u64 * image.height as u64 / out_height as u64) as u32;
        let bottom = (((row + 1) as u64 * image.height as u64).div_ceil(out_height as u64) as u32)
            .min(image.height)
            .max(top + 1);

        for column in 0..out_width {
            let left = (column as u64 * image.width as u64 / out_width as u64) as u32;
            let right = (((column + 1) as u64 * image.width as u64).div_ceil(out_width as u64) as u32)
                .min(image.width)
                .max(left + 1);

            let mut totals = [0u64; 4];
            let mut counted = 0u64;
            for y in top..bottom {
                for x in left..right {
                    let base = ((y as usize * image.width as usize) + x as usize) * 4;
                    for (total, sample) in totals.iter_mut().zip(&source[base..base + 4]) {
                        *total += *sample as u64;
                    }
                    counted += 1;
                }
            }

            for total in totals {
                out.push((total / counted.max(1)) as u8);
            }
        }
    }

    DecodedImage {
        width: out_width,
        height: out_height,
        pixels: out,
        depth: Depth::Eight,
    }
}

/// Encode as a JPEG that fits inside `budget` bytes, if one can be made.
///
/// Quality is searched rather than guessed: what a given quality costs depends
/// entirely on the picture — a photograph of foliage and a screenshot of text
/// at quality 80 differ by an order of magnitude — so asking the encoder is the
/// only way to know. A binary search over the quality range answers in seven
/// encodes at most.
///
/// Returns the best fit found, and the quality it used. `None` when even the
/// lowest quality is too large, which the caller answers by shrinking the
/// picture instead of lying about the size.
pub fn within_budget(request: &Request<'_>, budget: usize) -> Result<Option<(Vec<u8>, u8)>> {
    let mut low = 1u8;
    let mut high = 100u8;
    let mut best: Option<(Vec<u8>, u8)> = None;

    while low <= high {
        let quality = low + (high - low) / 2;
        let attempt = encode(&Request { quality, ..copy_of(request) })?;

        if attempt.len() <= budget {
            // It fits: keep it and ask whether a better-looking one also does.
            best = Some((attempt, quality));
            low = quality + 1;
        } else {
            if quality == 1 {
                break;
            }
            high = quality - 1;
        }
    }

    Ok(best)
}

/// A `Request` with the same borrows, so the budget search can vary one field.
fn copy_of<'a>(request: &Request<'a>) -> Request<'a> {
    Request {
        image: request.image,
        profile: request.profile,
        target: request.target,
        colour: request.colour,
        quality: request.quality,
        caveat: request.caveat,
    }
}

/// Eight-bit RGBA, with the colour taken wherever the request asks for it.
fn to_eight_bit(request: &Request<'_>) -> Vec<u8> {
    match request.colour {
        Colour::KeepProfile => narrow(request.image),
        Colour::BakeToSrgb => to_srgb(request.image, request.profile),
    }
}

/// Sixteen-bit samples down to eight, unchanged otherwise.
fn narrow(image: &DecodedImage) -> Vec<u8> {
    match image.depth {
        Depth::Eight => image.pixels.clone(),
        Depth::Sixteen => image
            .pixels
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| {
                let value = u16::from_ne_bytes([sample[0], sample[1]]);
                // Round rather than truncate: the alternative darkens every
                // sample by up to one part in 256, which over a gradient is a
                // visible shift rather than a rounding detail.
                ((value as u32 * 255 + 32767) / 65535) as u8
            })
            .collect(),
    }
}

/// The shader's colour path, run over every pixel.
///
/// Stored values to linear light through the image's own curves, between
/// primaries by the 3x3 matrix, clamped to what a standard-range surface can
/// carry, and re-encoded as sRGB. The clamp is where an HDR picture loses its
/// highlights, and it is the same clamp the screen does.
fn to_srgb(image: &DecodedImage, profile: Option<&moxcms::ColorProfile>) -> Vec<u8> {
    let srgb = moxcms::ColorProfile::new_srgb();
    let transform = ColorTransform::for_image(profile, &srgb);

    // The shader skips the whole conversion when it would change nothing, and
    // so does this: an sRGB picture put through decode-and-re-encode would come
    // back a rounding step away from where it started, which is a change for
    // nothing. Untagged files land here too — they state no colour, so there is
    // nothing to convert them from (ADR 0005).
    if transform.is_identity {
        return narrow(image);
    }

    let samples = match image.depth {
        Depth::Eight => image.pixels.len(),
        Depth::Sixteen => image.pixels.len() / 2,
    };
    let mut out = Vec::with_capacity(samples);

    let read = |index: usize| -> f32 {
        match image.depth {
            Depth::Eight => image.pixels[index] as f32 / 255.0,
            Depth::Sixteen => u16::from_ne_bytes([image.pixels[index * 2], image.pixels[index * 2 + 1]]) as f32 / 65535.0,
        }
    };

    for pixel in 0..samples / 4 {
        let base = pixel * 4;
        let linear = [
            transform.decode_channel(read(base), 0),
            transform.decode_channel(read(base + 1), 1),
            transform.decode_channel(read(base + 2), 2),
        ];
        let converted = transform.to_display(linear);
        for channel in converted {
            out.push(encode_srgb_channel(channel));
        }
        // Alpha is a coverage, not a colour: it goes through no curve and no
        // matrix, on the GPU or here.
        out.push((read(base + 3) * 255.0).round().clamp(0.0, 255.0) as u8);
    }

    out
}

/// The sRGB transfer function, linear light to a stored byte.
///
/// The clamp is the shader's: on a standard-range surface a colour outside
/// 0..1 — an HDR highlight, or a gamut the display cannot reach — is clipped
/// rather than compressed.
fn encode_srgb_channel(value: f32) -> u8 {
    let clamped = value.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Composite RGBA onto white, dropping the alpha channel.
///
/// White rather than the viewer's own backdrop: the file is leaving, and it
/// should not carry a colour that belongs to the program that wrote it.
fn over_white(pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() / 4 * 3);
    for pixel in pixels.as_chunks::<4>().0 {
        let alpha = pixel[3] as u32;
        for channel in &pixel[..3] {
            // Over white: the picture's own colour at `alpha`, white for the
            // rest. Rounded, so a fully opaque pixel comes out exactly as it
            // went in.
            out.push(((*channel as u32 * alpha + 255 * (255 - alpha) + 127) / 255) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with a known value in every channel.
    fn picture(width: u32, height: u32, pixel: [u8; 4]) -> DecodedImage {
        DecodedImage {
            width,
            height,
            pixels: pixel.repeat((width * height) as usize),
            depth: Depth::Eight,
        }
    }

    fn request<'a>(image: &'a DecodedImage, target: Target) -> Request<'a> {
        Request {
            image,
            profile: None,
            target,
            colour: Colour::KeepProfile,
            quality: 90,
            caveat: None,
        }
    }

    /// The three targets are what the interface offers and what the README
    /// lists, and a fourth added in one place has to reach the others.
    #[test]
    fn every_target_has_an_extension_that_names_it_back() {
        for target in Target::ALL {
            assert_eq!(
                Target::from_extension(target.extension()),
                Some(*target),
                "{} does not survive a round trip through its extension",
                target.label()
            );
        }
    }

    /// An empty picture is refused rather than written as a file nothing can
    /// open.
    #[test]
    fn a_picture_with_no_pixels_is_an_error() {
        let empty = DecodedImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            depth: Depth::Eight,
        };
        assert!(encode(&request(&empty, Target::Png)).is_err());
    }

    /// Every target writes bytes its own decoder recognises. Written by us,
    /// read by the crate that reads other people's files.
    #[test]
    fn each_target_writes_a_file_the_viewer_can_open_again() {
        let image = picture(8, 4, [200, 100, 50, 255]);
        for target in Target::ALL {
            let bytes = encode(&request(&image, *target)).unwrap_or_else(|error| panic!("{}: {error:#}", target.label()));
            let format = crate::format::Format::detect(&bytes).unwrap_or_else(|| panic!("{} wrote bytes nothing recognises", target.label()));
            let expected = match target {
                Target::Jpeg => crate::format::Format::Jpeg,
                Target::Png => crate::format::Format::Png,
                Target::WebP => crate::format::Format::WebP,
            };
            assert_eq!(format, expected, "{} wrote a {format:?}", target.label());
        }
    }

    /// PNG is lossless, so a picture written as one comes back exactly.
    ///
    /// This is the gate on the pixel path itself: a mistake in the row stride,
    /// the channel order or the alpha would show here rather than in a file
    /// somebody opens next week.
    #[test]
    fn a_png_comes_back_pixel_for_pixel() {
        let mut image = picture(6, 3, [0, 0, 0, 255]);
        for (index, pixel) in image.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            pixel[0] = (index * 9) as u8;
            pixel[1] = (index * 5 + 20) as u8;
            pixel[2] = (255 - index * 7) as u8;
            pixel[3] = 255;
        }

        let bytes = encode(&request(&image, Target::Png)).expect("encoding a PNG");
        let read = image::load_from_memory(&bytes).expect("reading it back").to_rgba8();

        assert_eq!(read.width(), image.width);
        assert_eq!(read.height(), image.height);
        assert_eq!(read.as_raw(), &image.pixels, "the pixels changed on the way out and back");
    }

    /// Transparency survives the formats that carry it.
    #[test]
    fn a_transparent_pixel_stays_transparent_in_a_png() {
        let image = picture(4, 4, [10, 20, 30, 0]);
        let bytes = encode(&request(&image, Target::Png)).expect("encoding a PNG");
        let read = image::load_from_memory(&bytes).expect("reading it back").to_rgba8();
        assert_eq!(read.get_pixel(0, 0)[3], 0, "the alpha channel was dropped");
    }

    /// JPEG has no alpha, so a transparent picture is composited rather than
    /// written with a channel the format cannot hold.
    #[test]
    fn a_transparent_picture_saved_as_jpeg_is_filled_with_white() {
        let image = picture(8, 8, [0, 0, 0, 0]);
        let bytes = encode(&request(&image, Target::Jpeg)).expect("encoding a JPEG");
        let read = image::load_from_memory(&bytes).expect("reading it back").to_rgb8();
        let pixel = read.get_pixel(4, 4);
        assert!(
            pixel[0] > 250 && pixel[1] > 250 && pixel[2] > 250,
            "a fully transparent picture came out as {pixel:?} rather than white"
        );
    }

    /// An opaque pixel must survive compositing untouched — the rounding in
    /// `over_white` is there for exactly this.
    #[test]
    fn an_opaque_pixel_is_not_moved_by_compositing() {
        let composited = over_white(&[7, 128, 249, 255]);
        assert_eq!(composited, vec![7, 128, 249]);
    }

    /// Sixteen-bit samples reach eight bits at their ends rather than near
    /// them: truncation would put full white at 254.
    #[test]
    fn sixteen_bit_white_narrows_to_eight_bit_white() {
        let image = DecodedImage {
            width: 1,
            height: 1,
            pixels: [0xFFFFu16, 0x0000, 0x8080, 0xFFFF].iter().flat_map(|value| value.to_ne_bytes()).collect(),
            depth: Depth::Sixteen,
        };
        assert_eq!(narrow(&image), vec![255, 0, 128, 255]);
    }

    /// The warnings say what will be lost, and only when it will be.
    #[test]
    fn a_warning_is_given_for_transparency_a_target_cannot_hold() {
        let clear = picture(2, 2, [1, 2, 3, 128]);
        let said = warnings(&request(&clear, Target::Jpeg));
        assert!(said.iter().any(|line| line.contains("transparency")), "JPEG said nothing about alpha: {said:?}");

        let solid = picture(2, 2, [1, 2, 3, 255]);
        let said = warnings(&request(&solid, Target::Jpeg));
        assert!(
            !said.iter().any(|line| line.contains("transparency")),
            "an opaque picture was warned about alpha: {said:?}"
        );
    }

    /// Baking an untagged picture changes nothing: it states no colour, so
    /// there is nothing to convert it from (ADR 0005).
    #[test]
    fn baking_an_untagged_picture_leaves_its_numbers_alone() {
        let image = picture(4, 2, [64, 128, 192, 255]);
        let baked = to_srgb(&image, None);
        assert_eq!(baked, image.pixels, "an untagged picture was converted from a colour it never claimed");
    }

    /// The point of baking: a picture in a wider space comes out with
    /// different numbers, holding the same colour.
    #[test]
    fn baking_a_wide_gamut_picture_moves_its_numbers() {
        let profile = moxcms::ColorProfile::new_display_p3();
        let image = picture(2, 2, [255, 0, 0, 255]);
        let baked = to_srgb(&image, Some(&profile));

        // P3 red is outside sRGB, so it clips to sRGB red — and the green and
        // blue it picks up on the way are what says a conversion happened.
        let pixel = &baked[..4];
        assert_eq!(pixel[0], 255, "red did not stay at the top of the range");
        assert_eq!(pixel[3], 255, "alpha was touched by the colour transform");
        assert!(
            baked.as_chunks::<4>().0.iter().all(|pixel| pixel[..3] == baked[..3]),
            "a picture of one colour came out in several"
        );
    }

    /// An HDR picture is announced as such before it is written, because the
    /// highlights it loses cannot be got back.
    #[test]
    fn an_hdr_picture_is_warned_about() {
        // BT.2020 primaries, PQ transfer, BT.2020 non-constant luminance —
        // the codes by number, as a real HDR file states them.
        let cicp = moxcms::CicpProfile {
            color_primaries: 9u8.try_into().unwrap(),
            transfer_characteristics: 16u8.try_into().unwrap(),
            matrix_coefficients: 9u8.try_into().unwrap(),
            full_range: true,
        };
        let mut profile = moxcms::ColorProfile::new_srgb();
        profile.update_rgb_colorimetry_from_cicp(cicp);

        let image = picture(2, 2, [200, 200, 200, 255]);
        let request = Request {
            image: &image,
            profile: Some(&profile),
            target: Target::Jpeg,
            colour: Colour::BakeToSrgb,
            quality: 90,
            caveat: None,
        };

        assert!(is_hdr(Some(&profile)), "a PQ profile was not recognised as HDR");
        assert!(
            warnings(&request).iter().any(|line| line.contains("HDR")),
            "an HDR picture was written without a word about it"
        );
    }

    /// The promise of the HDR export, measured on pixels rather than stated in
    /// a warning: reference white comes out white, and light above it comes out
    /// the same white, because that is what the screen does with it.
    ///
    /// The clamp is the whole of it. Without one, light above reference white
    /// wraps, saturates or overflows on its way to a byte — anything but the
    /// flat white a standard-range surface shows — so removing it from either
    /// this or the shader fails here.
    #[test]
    fn hdr_light_above_white_clips_to_white_just_as_the_screen_shows_it() {
        let cicp = moxcms::CicpProfile {
            color_primaries: 9u8.try_into().unwrap(),
            transfer_characteristics: 16u8.try_into().unwrap(),
            matrix_coefficients: 9u8.try_into().unwrap(),
            full_range: true,
        };
        let mut profile = moxcms::ColorProfile::new_srgb();
        profile.update_rgb_colorimetry_from_cicp(cicp);

        // PQ is absolute: a stored value names a number of nits. 0.58 is very
        // near BT.2408 reference white, 203 nits, and 0.75 is far above it —
        // the sort of specular highlight an HDR screen shows and an SDR one
        // cannot.
        let white = to_srgb(&picture(1, 1, [148, 148, 148, 255]), Some(&profile));
        let brighter = to_srgb(&picture(1, 1, [191, 191, 191, 255]), Some(&profile));

        assert!(
            white[0] > 235,
            "PQ reference white came out at {} rather than near the top of the range",
            white[0]
        );
        assert_eq!(
            brighter[..3],
            [255, 255, 255],
            "light above reference white came out as {:?} rather than clipped to white",
            &brighter[..3]
        );

        // The half of this a tone-map operator would change, and the reason it
        // is asserted: an operator earns its highlights by darkening the
        // mid-tones to make room. Clipping does not, so a mid-tone must come
        // out where the curve alone puts it. Measured through this very path,
        // so a change to the curves moves it and says so.
        let middle = to_srgb(&picture(1, 1, [128, 128, 128, 255]), Some(&profile));
        assert!(
            (middle[0] as i32 - 180).abs() <= 2,
            "a PQ mid-tone came out at {} rather than 180: something is shaping the curve, not clipping it",
            middle[0]
        );
    }

    /// Shrinking holds the shape of the picture, and stops at the bound
    /// rather than near it.
    #[test]
    fn shrinking_lands_on_the_bound_and_keeps_the_proportions() {
        let image = picture(800, 400, [10, 20, 30, 255]);
        let small = shrink(&image, 200);
        assert_eq!((small.width, small.height), (200, 100));
        assert_eq!(small.pixels.len(), 200 * 100 * 4);
    }

    /// A picture already small enough is left alone. Enlarging it would be
    /// inventing detail nobody asked for.
    #[test]
    fn a_picture_inside_the_bound_is_not_touched() {
        let image = picture(120, 80, [1, 2, 3, 255]);
        let same = shrink(&image, 400);
        assert_eq!((same.width, same.height), (120, 80));
        assert_eq!(same.pixels, image.pixels);
    }

    /// The averaging is the point: half black and half white must come back
    /// grey, not as whichever pixel a nearest-neighbour walk happened to land
    /// on. That is the difference between a readable small copy and a mess of
    /// stair-steps, and it is what a nearest-neighbour shrink would fail.
    #[test]
    fn shrinking_averages_rather_than_picking_one_pixel() {
        let mut image = picture(4, 1, [0, 0, 0, 255]);
        for (index, pixel) in image.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let shade = if index % 2 == 0 { 0 } else { 255 };
            pixel[0] = shade;
            pixel[1] = shade;
            pixel[2] = shade;
        }

        let small = shrink(&image, 2);
        assert_eq!((small.width, small.height), (2, 1));
        for pixel in small.pixels.as_chunks::<4>().0 {
            assert!(
                (100..=155).contains(&pixel[0]),
                "black beside white averaged to {} rather than to grey",
                pixel[0]
            );
        }
    }

    /// The budget search returns the best-looking file that fits, and says
    /// what quality it settled on.
    #[test]
    fn a_budget_is_met_with_the_best_quality_that_fits() {
        // Noise, so quality actually costs bytes: a flat colour compresses to
        // almost nothing at every setting and would make any budget look met.
        let mut image = picture(160, 160, [0, 0, 0, 255]);
        let mut seed = 12345u32;
        for pixel in image.pixels.as_chunks_mut::<4>().0.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            pixel[0] = (seed >> 16) as u8;
            pixel[1] = (seed >> 8) as u8;
            pixel[2] = seed as u8;
        }

        let request = Request {
            image: &image,
            profile: None,
            target: Target::Jpeg,
            colour: Colour::KeepProfile,
            quality: 90,
            caveat: None,
        };

        let budget = 6000;
        let (bytes, quality) = within_budget(&request, budget).expect("searching").expect("some quality fits");
        assert!(bytes.len() <= budget, "the search returned {} bytes for a budget of {budget}", bytes.len());

        // And it is the best that fits: one step up must not.
        if quality < 100 {
            let bigger = encode(&Request {
                quality: quality + 1,
                ..copy_of(&request)
            })
            .expect("encoding one step up");
            assert!(
                bigger.len() > budget,
                "quality {} fits in {budget} too, so the search settled too low",
                quality + 1
            );
        }
    }

    /// A budget nothing can meet is said so rather than answered with a file
    /// that breaks it.
    #[test]
    fn a_budget_too_small_for_any_quality_is_refused() {
        let mut image = picture(200, 200, [0, 0, 0, 255]);
        let mut seed = 999u32;
        for pixel in image.pixels.as_chunks_mut::<4>().0.iter_mut() {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            pixel[0] = (seed >> 16) as u8;
            pixel[1] = (seed >> 8) as u8;
            pixel[2] = seed as u8;
        }

        let request = Request {
            image: &image,
            profile: None,
            target: Target::Jpeg,
            colour: Colour::KeepProfile,
            quality: 90,
            caveat: None,
        };
        assert_eq!(within_budget(&request, 200).expect("searching"), None, "a 200-byte budget was somehow met");
    }

    /// The colour path here and the one in the shader must not drift apart.
    ///
    /// Read out of the WGSL rather than remembered: the shader is the thing a
    /// person sees, so it is the thing this has to match. A change to its
    /// standard-range output — a tone curve where the clamp is, a different
    /// sRGB encoding — fails here, which is the moment to decide whether the
    /// export should follow it.
    #[test]
    fn holds_the_same_colour_as_the_shader() {
        let shader = include_str!("gpu/shader.wgsl");

        assert!(
            shader.contains("let clipped = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));"),
            "the shader no longer clamps a standard-range surface; the export still does"
        );
        assert!(
            shader.contains("return clamped * 12.92;") && shader.contains("return 1.055 * pow(clamped, 1.0 / 2.4) - 0.055;"),
            "the shader's sRGB encoding changed; the export still writes the old one"
        );
        assert!(
            shader.contains("rgb = colour.matrix * linear;"),
            "the shader no longer converts between primaries by the matrix; the export still does"
        );
    }
}
