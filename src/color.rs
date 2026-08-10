//! Turning an image's colour profile into something a shader can apply.
//!
//! An untagged image is assumed to be sRGB, which is what every other viewer
//! assumes and what the overwhelming majority of untagged files actually are.
//! A tagged one is converted from its own primaries to the display's, on the
//! GPU, per frame.
//!
//! Doing this in the shader rather than at decode time is deliberate: the
//! conversion is then free (it rides along with sampling that happens anyway),
//! it does not delay the first frame the viewer is measured on, and the
//! decoded pixels stay as the file stored them, so a later change of display
//! profile costs a redraw rather than a re-decode.

use moxcms::{ColorProfile, ToneReprCurve};

/// How many entries the tone curves are sampled into.
///
/// 1024 is past the point where banding is visible in an 8-bit image, and the
/// three curves together come to 12 KB — small enough not to think about.
pub const CURVE_SAMPLES: usize = 1024;

/// A profile reduced to what the shader needs.
///
/// The conversion is: decode each channel through its tone curve into light,
/// multiply by a 3x3 matrix, then re-encode for the display. This carries the
/// pieces of that in the form the GPU wants them.
#[derive(Clone, Debug)]
pub struct ColorTransform {
    /// Row-major matrix taking linear source RGB to linear display RGB.
    pub matrix: [[f32; 3]; 3],
    /// Source tone curves sampled to linear light, one row per channel.
    pub decode: Vec<f32>,
    /// Whether this is the identity — an sRGB image on an sRGB display.
    pub is_identity: bool,
}

impl ColorTransform {
    /// The transform that changes nothing.
    ///
    /// Used when the image and the display agree, which is the common case and
    /// the one that must cost nothing.
    pub fn identity() -> Self {
        Self {
            matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            decode: srgb_to_linear_curve(),
            is_identity: true,
        }
    }

    /// Build the transform from an image profile to a display profile.
    pub fn new(source: &ColorProfile, display: &ColorProfile) -> Self {
        let matrix = source.transform_matrix(display);
        let matrix = [
            [matrix.v[0][0] as f32, matrix.v[0][1] as f32, matrix.v[0][2] as f32],
            [matrix.v[1][0] as f32, matrix.v[1][1] as f32, matrix.v[1][2] as f32],
            [matrix.v[2][0] as f32, matrix.v[2][1] as f32, matrix.v[2][2] as f32],
        ];

        let decode = sample_curves(source);
        // Compared with a tolerance, not for equality: a profile's sRGB curve
        // arrives as a sampled table or as parametric coefficients, and either
        // reconstructs the analytic curve to within a hair rather than exactly.
        // Demanding bit equality would put every ordinary image through a
        // conversion that changes nothing but costs a texture and a matrix.
        let is_identity = is_near_identity(&matrix) && is_near_srgb_curve(&decode);

        Self { matrix, decode, is_identity }
    }
}

/// The profile Windows has assigned to the display.
///
/// Falls back to sRGB, which is what an unprofiled monitor is closest to and
/// what every other viewer assumes anyway.
pub fn display_profile() -> ColorProfile {
    #[cfg(windows)]
    if let Some(profile) = windows_display_profile() {
        return profile;
    }
    ColorProfile::new_srgb()
}

/// Read the ICC profile Windows associates with the primary display.
#[cfg(windows)]
fn windows_display_profile() -> Option<ColorProfile> {
    use windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC};
    use windows::Win32::UI::ColorSystem::GetICMProfileW;
    use windows::core::{PWSTR, w};

    // SAFETY: `CreateDCW` on the DISPLAY driver returns a device context for
    // the primary display, or null. Every path below releases it.
    let context = unsafe { CreateDCW(w!("DISPLAY"), None, None, None) };
    if context.is_invalid() {
        return None;
    }

    let mut length = 0u32;
    // The first call reports the length the path needs, and is expected to
    // fail with that length written out.
    let _ = unsafe { GetICMProfileW(context, &mut length, None) };

    let mut buffer = vec![0u16; length.max(1) as usize];
    let read = unsafe { GetICMProfileW(context, &mut length, Some(PWSTR(buffer.as_mut_ptr()))) };
    let _ = unsafe { DeleteDC(context) };
    if !read.as_bool() {
        return None;
    }

    let end = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    let path = String::from_utf16(&buffer[..end]).ok()?;

    let bytes = std::fs::read(path).ok()?;
    ColorProfile::new_from_slice(&bytes).ok()
}

/// Read the ICC profile a file carries, if it carries one.
///
/// Returns `None` for an untagged file, which is not an error: it means sRGB
/// by convention, and that is what the caller falls back to.
pub fn profile_from(bytes: &[u8]) -> Option<ColorProfile> {
    let raw = raw_profile(bytes)?;
    // A malformed profile is treated as no profile. Refusing to show an image
    // because its colour metadata is broken serves nobody.
    ColorProfile::new_from_slice(&raw).ok()
}

/// Extract the raw ICC bytes from a JPEG or a PNG.
fn raw_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // zune-jpeg already walks the APP segments while decoding headers, so
        // no second parser is needed for the format the viewer opens most.
        let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
        decoder.decode_headers().ok()?;
        return decoder.icc_profile();
    }

    png_profile(bytes)
}

/// Pull an `iCCP` chunk out of a PNG.
///
/// Written by hand because the `image` crate exposes the profile only through
/// a decoder built for the pixels, which would mean decoding the image twice.
fn png_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if !bytes.starts_with(&SIGNATURE) {
        return None;
    }

    let mut offset = SIGNATURE.len();
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let data_start = offset + 8;
        let data_end = data_start.checked_add(length)?;
        if data_end > bytes.len() {
            return None;
        }

        match kind {
            b"iCCP" => {
                let data = &bytes[data_start..data_end];
                // Profile name, a zero, a compression byte, then zlib data.
                let zero = data.iter().position(|byte| *byte == 0)?;
                let compressed = data.get(zero + 2..)?;
                return inflate(compressed);
            }
            // Nothing before the pixels means there is no profile to find.
            b"IDAT" | b"IEND" => return None,
            _ => {}
        }

        // Chunk length, type, data, and a four-byte CRC.
        offset = data_end + 4;
    }

    None
}

/// Inflate a zlib stream, using the decompressor the `image` crate already
/// brings in for PNG.
fn inflate(compressed: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(compressed).read_to_end(&mut out).ok()?;
    Some(out)
}

/// Sample a profile's tone curves into a lookup the shader can read.
///
/// Each channel gets `CURVE_SAMPLES` entries mapping the stored value to
/// linear light. Sampling rather than evaluating means an arbitrary curve —
/// a scanner profile, a camera's own — costs the same as a simple gamma.
fn sample_curves(profile: &ColorProfile) -> Vec<f32> {
    let curves = [&profile.red_trc, &profile.green_trc, &profile.blue_trc];
    let mut out = Vec::with_capacity(CURVE_SAMPLES * 3);

    for curve in curves {
        for index in 0..CURVE_SAMPLES {
            let position = index as f32 / (CURVE_SAMPLES - 1) as f32;
            out.push(match curve {
                Some(curve) => evaluate(curve, position),
                // A profile with no curve for a channel is taken as sRGB, the
                // same assumption an untagged file gets.
                None => srgb_to_linear(position),
            });
        }
    }

    out
}

/// Evaluate a tone curve at a position in `0..=1`.
fn evaluate(curve: &ToneReprCurve, position: f32) -> f32 {
    match curve {
        ToneReprCurve::Lut(lut) => match lut.len() {
            // An empty curve means the identity; a single entry is a gamma
            // value stored in u8Fixed8 format.
            0 => position,
            1 => position.powf(f32::from(lut[0]) / 256.0),
            _ => interpolate(lut, position),
        },
        ToneReprCurve::Parametric(parameters) => parametric(parameters, position),
    }
}

/// Read a sampled curve with linear interpolation between entries.
fn interpolate(lut: &[u16], position: f32) -> f32 {
    let last = lut.len() - 1;
    let scaled = position.clamp(0.0, 1.0) * last as f32;
    let index = scaled.floor() as usize;
    let fraction = scaled - index as f32;

    let low = f32::from(lut[index.min(last)]) / 65535.0;
    let high = f32::from(lut[(index + 1).min(last)]) / 65535.0;
    low + (high - low) * fraction
}

/// Evaluate an ICC parametric curve (types 0 through 4).
///
/// The parameter order follows the ICC specification's `parametricCurveType`.
fn parametric(parameters: &[f32], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    match parameters {
        // Type 0: Y = X^g
        [g] => x.powf(*g),
        // Type 1: a CIE 122 curve.
        [g, a, b] => {
            if x >= -b / a {
                (a * x + b).powf(*g)
            } else {
                0.0
            }
        }
        // Type 2: an IEC 61966-3 curve.
        [g, a, b, c] => {
            if x >= -b / a {
                (a * x + b).powf(*g) + c
            } else {
                *c
            }
        }
        // Type 3: the shape sRGB and Rec. 709 use.
        [g, a, b, c, d] => {
            if x >= *d {
                (a * x + b).powf(*g)
            } else {
                c * x
            }
        }
        // Type 4: the same with offsets on both segments.
        [g, a, b, c, d, e, f] => {
            if x >= *d {
                (a * x + b).powf(*g) + e
            } else {
                c * x + f
            }
        }
        // Anything else is a curve shape this build does not know; sRGB is a
        // better guess than a black image.
        _ => srgb_to_linear(x),
    }
}

/// The sRGB transfer function, encoded to linear.
fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB curve sampled the same way a profile's would be.
fn srgb_to_linear_curve() -> Vec<f32> {
    let mut out = Vec::with_capacity(CURVE_SAMPLES * 3);
    for _ in 0..3 {
        for index in 0..CURVE_SAMPLES {
            out.push(srgb_to_linear(index as f32 / (CURVE_SAMPLES - 1) as f32));
        }
    }
    out
}

/// Whether a sampled curve is the sRGB one to within 8-bit precision.
///
/// Half a level of an 8-bit channel is 1/512; the tolerance is a shade under
/// that, so a curve that would round to the same stored value counts as sRGB.
fn is_near_srgb_curve(decode: &[f32]) -> bool {
    let reference = srgb_to_linear_curve();
    decode.len() == reference.len() && decode.iter().zip(&reference).all(|(actual, expected)| (actual - expected).abs() < 0.002)
}

fn is_near_identity(matrix: &[[f32; 3]; 3]) -> bool {
    for (row, values) in matrix.iter().enumerate() {
        for (column, value) in values.iter().enumerate() {
            let expected = if row == column { 1.0 } else { 0.0 };
            // Half a percent: below what an 8-bit channel can express.
            if (value - expected).abs() > 0.005 {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untagged_file_has_no_profile() {
        assert!(profile_from(b"not an image").is_none());
        assert!(profile_from(&[]).is_none());
    }

    #[test]
    fn srgb_on_srgb_is_the_identity() {
        let srgb = ColorProfile::new_srgb();
        let transform = ColorTransform::new(&srgb, &srgb);

        assert!(
            is_near_identity(&transform.matrix),
            "the matrix should be the identity, got {:?}",
            transform.matrix
        );

        let reference = srgb_to_linear_curve();
        let worst = transform
            .decode
            .iter()
            .zip(&reference)
            .map(|(actual, expected)| (actual - expected).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.002, "the curve differs from sRGB by {worst}");

        assert!(transform.is_identity, "an sRGB image on an sRGB display must not be converted");
    }

    /// The case the feature exists for: a wide-gamut image on a normal display
    /// has to be brought in, or its colours come out oversaturated.
    #[test]
    fn display_p3_on_srgb_is_a_real_conversion() {
        let transform = ColorTransform::new(&ColorProfile::new_display_p3(), &ColorProfile::new_srgb());
        assert!(!transform.is_identity, "P3 to sRGB must not be the identity");

        // A saturated green in P3 is outside sRGB, so the matrix has to pull
        // the other channels negative to represent it.
        assert!(
            transform.matrix[0][1] < 0.0,
            "expected a negative red-from-green term, got {:?}",
            transform.matrix
        );
    }

    #[test]
    fn the_curves_cover_every_channel() {
        let transform = ColorTransform::new(&ColorProfile::new_srgb(), &ColorProfile::new_srgb());
        assert_eq!(transform.decode.len(), CURVE_SAMPLES * 3);
    }

    #[test]
    fn a_decode_curve_runs_from_black_to_white() {
        let transform = ColorTransform::identity();
        assert!(transform.decode[0].abs() < 0.001, "black must stay black");
        assert!((transform.decode[CURVE_SAMPLES - 1] - 1.0).abs() < 0.001, "white must stay white");
    }

    #[test]
    fn the_srgb_curve_matches_its_definition_at_the_midpoint() {
        // 0.5 encoded is a little under a quarter of the light: the standard
        // figure, and the one a wrong exponent would miss.
        assert!((srgb_to_linear(0.5) - 0.2140).abs() < 0.001);
    }

    #[test]
    fn a_parametric_srgb_curve_matches_the_analytic_one() {
        // Type 3 with the sRGB parameters.
        let srgb = [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.040_45];
        for step in 0..=10 {
            let x = step as f32 / 10.0;
            let difference = (parametric(&srgb, x) - srgb_to_linear(x)).abs();
            assert!(difference < 0.001, "at {x}: {difference}");
        }
    }

    #[test]
    fn a_single_entry_lut_is_a_gamma_value() {
        // 2.2 in u8Fixed8 is 563; the curve should behave as x^2.2.
        let curve = ToneReprCurve::Lut(vec![563]);
        let expected = 0.5f32.powf(563.0 / 256.0);
        assert!((evaluate(&curve, 0.5) - expected).abs() < 0.001);
    }

    #[test]
    fn a_sampled_lut_interpolates_between_entries() {
        let curve = ToneReprCurve::Lut(vec![0, 32768, 65535]);
        assert!((evaluate(&curve, 0.0) - 0.0).abs() < 0.001);
        assert!((evaluate(&curve, 0.5) - 0.5).abs() < 0.01);
        assert!((evaluate(&curve, 1.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn an_unknown_curve_shape_falls_back_to_srgb() {
        // Two parameters match no ICC parametric type.
        assert!((parametric(&[1.0, 2.0], 0.5) - srgb_to_linear(0.5)).abs() < 0.001);
    }

    /// The conversion has to be applied in the right direction. A saturated
    /// green in Display P3 is more saturated than sRGB can show; converting it
    /// must pull it *in*, not push it further out.
    #[test]
    fn a_wide_gamut_green_comes_in_towards_srgb() {
        let transform = ColorTransform::new(&ColorProfile::new_display_p3(), &ColorProfile::new_srgb());

        // Full green in the source, already linear.
        let source = [0.0f32, 1.0, 0.0];
        let converted = apply(&transform.matrix, source);

        // Out of sRGB's reach: red and blue go negative to represent a green
        // the display cannot make.
        assert!(converted[1] > 0.8, "green should stay dominant: {converted:?}");
        assert!(
            converted[0] < 0.0 || converted[2] < 0.0,
            "a P3 green is outside sRGB and must show as out of range: {converted:?}"
        );
    }

    /// Grey has no hue in any RGB space, so a conversion between two profiles
    /// with the same white point must leave it grey — a classic sign of a
    /// matrix applied the wrong way round is a colour cast on neutrals.
    #[test]
    fn neutral_grey_survives_a_conversion_unchanged() {
        let transform = ColorTransform::new(&ColorProfile::new_display_p3(), &ColorProfile::new_srgb());
        let grey = apply(&transform.matrix, [0.5, 0.5, 0.5]);

        assert!(
            (grey[0] - grey[1]).abs() < 0.005 && (grey[1] - grey[2]).abs() < 0.005,
            "grey picked up a colour cast: {grey:?}"
        );
        assert!((grey[0] - 0.5).abs() < 0.01, "grey changed brightness: {grey:?}");
    }

    /// Converting to a wider space and back has to return where it started.
    #[test]
    fn a_round_trip_through_a_wider_space_returns_the_original() {
        let srgb = ColorProfile::new_srgb();
        let p3 = ColorProfile::new_display_p3();

        let out = ColorTransform::new(&srgb, &p3);
        let back = ColorTransform::new(&p3, &srgb);

        for colour in [[0.2, 0.7, 0.4], [1.0, 0.0, 0.0], [0.35, 0.35, 0.9]] {
            let round_tripped = apply(&back.matrix, apply(&out.matrix, colour));
            for (channel, (actual, expected)) in round_tripped.iter().zip(&colour).enumerate() {
                assert!(
                    (actual - expected).abs() < 0.005,
                    "channel {channel} of {colour:?} came back as {round_tripped:?}"
                );
            }
        }
    }

    fn apply(matrix: &[[f32; 3]; 3], colour: [f32; 3]) -> [f32; 3] {
        let mut out = [0.0; 3];
        for (row, values) in matrix.iter().enumerate() {
            out[row] = values.iter().zip(&colour).map(|(coefficient, channel)| coefficient * channel).sum();
        }
        out
    }

    #[test]
    fn a_png_without_a_profile_yields_none() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(2, 2, image::Rgba([1, 2, 3, 255]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(profile_from(&png.into_inner()).is_none());
    }
}
