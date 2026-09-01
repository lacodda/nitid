//! Turning an image's colour profile into something a shader can apply.
//!
//! A tagged image is converted from its own primaries to the display's, on the
//! GPU, per frame. An untagged one is passed through untouched, the way every
//! other viewer on Windows shows it — see `ColorTransform::for_image`.
//!
//! Doing this in the shader rather than at decode time is deliberate: the
//! conversion is then free (it rides along with sampling that happens anyway),
//! it does not delay the first frame the viewer is measured on, and the
//! decoded pixels stay as the file stored them, so a later change of display
//! profile costs a redraw rather than a re-decode.

use moxcms::{ColorProfile, ProfileText, ToneReprCurve};

use crate::format::Format;

/// How many entries the tone curves are sampled into.
///
/// 1024 is past the point where banding is visible in an 8-bit image, and the
/// three curves together come to 12 KB — small enough not to think about.
pub const CURVE_SAMPLES: usize = 1024;

/// What is happening to this image's colour, in the words a person would use.
///
/// The status line has room for three words and says "converted"; this is what
/// those three words are short for. It exists because colour management is the
/// one thing a viewer does that is invisible when it works and inexplicable
/// when it does not: a photograph that looks wrong here and right elsewhere is
/// a question nobody can answer by looking harder at the picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Passport {
    /// What the file says its numbers mean, named as the profile names itself.
    ///
    /// `None` for an untagged file, which says nothing — see ADR 0005 for why
    /// that is passed through rather than assumed to be sRGB.
    pub source: Option<String>,
    /// What the display says it can show.
    pub display: Option<String>,
    /// Whether a conversion is actually happening.
    pub converting: bool,
    /// How far apart the two spaces are, as the largest single change the
    /// matrix makes to a primary.
    ///
    /// Zero for the identity. It is not a colorimetric error figure and is not
    /// presented as one: it answers "is this a big change or a small one",
    /// which is what someone reading a passport wants to know.
    pub distance: f32,
}

impl Passport {
    /// Describe the path this image's colour takes to the screen.
    pub fn new(source: Option<&ColorProfile>, display: &ColorProfile, transform: &ColorTransform) -> Self {
        Self {
            source: source.and_then(profile_name),
            display: profile_name(display),
            converting: !transform.is_identity,
            distance: matrix_distance(&transform.matrix),
        }
    }

    /// One line saying what is happening, for a panel that has room for a
    /// sentence rather than a word.
    pub fn summary(&self) -> String {
        match (&self.source, self.converting) {
            // An untagged file states nothing about its numbers, so nothing is
            // assumed and nothing is converted.
            (None, _) => "This file does not say what its colours mean, so they are shown untouched.".into(),
            (Some(source), false) => format!("{source} matches the display, so nothing is converted."),
            (Some(source), true) => {
                let display = self.display.as_deref().unwrap_or("the display");
                format!("{source} converted to {display}.")
            }
        }
    }
}

/// What a profile calls itself.
///
/// A profile carries its description in one of three encodings depending on
/// its age, and a viewer that read only the modern one would leave most
/// display profiles nameless — the ones Windows ships are old.
fn profile_name(profile: &ColorProfile) -> Option<String> {
    let text = profile.description.as_ref()?;
    let name = match text {
        ProfileText::PlainString(value) => value.clone(),
        // The localised form: any entry will do, because a viewer that picked
        // by language would still have to fall back to whatever is there.
        ProfileText::Localizable(entries) => entries.first()?.value.clone(),
        // The old form carries both, and the ASCII one is the one that is
        // always filled in.
        ProfileText::Description(description) => {
            if description.ascii_string.trim().is_empty() {
                description.unicode_string.clone()
            } else {
                description.ascii_string.clone()
            }
        }
    };

    let trimmed = name.trim().trim_end_matches('\0').trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// How far the matrix moves a colour, as the largest change to any one
/// primary.
///
/// The identity leaves every primary alone and measures zero; a conversion
/// between distant spaces moves them further. Deliberately a single number
/// with no unit attached: it is a sense of scale, not a measurement.
fn matrix_distance(matrix: &[[f32; 3]; 3]) -> f32 {
    let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut worst: f32 = 0.0;
    for (row, expected) in matrix.iter().zip(&identity) {
        for (value, expected) in row.iter().zip(expected) {
            worst = worst.max((value - expected).abs());
        }
    }
    worst
}

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
    /// Choose the transform for a file, given what profile it carries.
    ///
    /// A file with a profile states what its numbers mean, so it is converted
    /// to what the display can show — that is colour management, and it is the
    /// reason this module exists.
    ///
    /// A file *without* one states nothing, and gets passed through untouched.
    /// Assuming sRGB and converting from it sounds more principled and is
    /// worse: on a wide-gamut display it visibly desaturates every untagged
    /// image, and it disagrees with Windows, the shell preview and every
    /// browser, all of which send untagged pixels straight to the screen. An
    /// image would then look one way here and another way everywhere else,
    /// including in whatever program its author used to make it.
    ///
    /// See `docs/adr/0005-untagged-images-pass-through.md`.
    pub fn for_image(profile: Option<&ColorProfile>, display: &ColorProfile) -> Self {
        match profile {
            Some(profile) => Self::new(profile, display),
            None => Self::identity(),
        }
    }

    /// One channel through its tone curve, from a stored value to linear
    /// light.
    ///
    /// The same lookup the shader does — the curves are sampled into rows and
    /// read with linear interpolation — done on the CPU for the one pixel the
    /// eyedropper is asked about. Reading that pixel back off the GPU instead
    /// would stall the whole pipeline, and it is asked for on every mouse move
    /// while the eyedropper is up.
    pub fn decode_channel(&self, value: f32, channel: usize) -> f32 {
        let row = channel.min(2) * CURVE_SAMPLES;
        let Some(curve) = self.decode.get(row..row + CURVE_SAMPLES) else {
            return value;
        };

        let position = value.clamp(0.0, 1.0) * (CURVE_SAMPLES - 1) as f32;
        let below = position.floor() as usize;
        let above = (below + 1).min(CURVE_SAMPLES - 1);
        let fraction = position - below as f32;
        curve[below] + (curve[above] - curve[below]) * fraction
    }

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
        // PQ stores absolute light: its decoded curve reaches 1.0 at ten
        // thousand nits, so HDR reference white — 203 nits, BT.2408 — comes
        // out at two percent and the whole picture would be nearly black on
        // any surface. Scaling reference white onto 1.0 puts it on SDR white,
        // which is where both surfaces expect it: a standard-range display
        // clips the highlights above it, an HDR one drives them into its
        // headroom. The scale rides in the matrix, so the shader needs no
        // extra step. See `docs/adr/0014-pq-reference-white-lands-on-sdr-white.md`.
        let brighten = pq_scale(source);
        let matrix = source.transform_matrix(display);
        let matrix = [
            [
                matrix.v[0][0] as f32 * brighten,
                matrix.v[0][1] as f32 * brighten,
                matrix.v[0][2] as f32 * brighten,
            ],
            [
                matrix.v[1][0] as f32 * brighten,
                matrix.v[1][1] as f32 * brighten,
                matrix.v[1][2] as f32 * brighten,
            ],
            [
                matrix.v[2][0] as f32 * brighten,
                matrix.v[2][1] as f32 * brighten,
                matrix.v[2][2] as f32 * brighten,
            ],
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

/// How much a profile's linear light must be scaled so its reference white
/// lands on 1.0.
///
/// Only PQ needs this: its transfer function is absolute, normalised to ten
/// thousand nits, and BT.2408 puts HDR reference white at 203 of them. Every
/// other curve — sRGB, gamma, even HLG, which is scene-relative — already
/// treats 1.0 as its white.
fn pq_scale(profile: &ColorProfile) -> f32 {
    match profile.cicp {
        Some(cicp) if cicp.transfer_characteristics == moxcms::TransferCharacteristics::Smpte2084 => 10000.0 / 203.0,
        _ => 1.0,
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
/// Returns `None` for an untagged file, which is not an error: such a file
/// states nothing about its colour and is shown as it is.
pub fn profile_from(bytes: &[u8]) -> Option<ColorProfile> {
    let raw = raw_profile(bytes)?;
    // A malformed profile is treated as no profile. Refusing to show an image
    // because its colour metadata is broken serves nobody.
    ColorProfile::new_from_slice(&raw).ok()
}

/// Extract the raw ICC bytes a container carries.
///
/// Every format the viewer opens is asked here. A format left out of this match
/// would still decode and still display — just in the wrong colours, silently,
/// which is the failure this module exists to prevent.
fn raw_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    match Format::detect(bytes)? {
        Format::Jpeg => {
            // zune-jpeg already walks the APP segments while decoding headers,
            // so no second parser is needed for the format opened most.
            let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
            decoder.decode_headers().ok()?;
            decoder.icc_profile()
        }
        Format::Png => png_profile(bytes),
        Format::WebP => image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?.icc_profile().ok()?,
        // JPEG XL does carry a profile, and it is read — but by the decoder,
        // which reaches it in the pass that produces the pixels. Parsing the
        // codestream again here to answer the same question would double the
        // cost of opening the format. See `image_source::decode_jxl`.
        Format::JpegXl => None,
        // AVIF states its colour as CICP code points inside the AV1 bitstream,
        // and the decoder reads them in the pass that produces the pixels —
        // the same arrangement as JPEG XL. See `avif::profile_from`.
        Format::Avif => None,
        // HEIC states its colour one of two ways. With CICP codes — the usual
        // case — the decoder resolves the colour itself and the pixels arrive
        // as sRGB, so attaching anything would convert them twice; that is the
        // limitation ADR 0007 records. With an ICC profile, the decoder reads
        // no such thing, so the profile is read here and applied on the GPU
        // like any other format's. `image_source::decode_heic_with_colour`
        // decides which case a file is and makes the pixels match.
        Format::Heic => heic_profile(bytes),
        // SVG states colours as sRGB values in the markup itself; there is no
        // profile to read, and none to attach.
        Format::Svg => None,
        // GIF is sRGB by definition. BMP and TIFF can carry a profile, but
        // neither appears tagged in practice often enough to justify a parser.
        Format::Gif | Format::Bmp | Format::Tiff => None,
    }
}

/// Extract the ICC profile a HEIC embeds, if it has one rather than CICP
/// codes.
///
/// The `colr` box holds either, never both in a way that matters here: a file
/// stating CICP has had its colour resolved by the decoder already.
fn heic_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    let colour = crate::isobmff::colour_box(bytes)?;
    if !colour.is_icc {
        return None;
    }

    // The kind tag, then the profile itself, to the end of the box.
    let body = crate::isobmff::find_box(bytes, b"colr")?;
    body.get(4..).map(<[u8]>::to_vec)
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

    /// The case the passport exists for: a file in one space shown on a
    /// display in another. It names both ends and says a conversion is under
    /// way.
    #[test]
    fn a_converted_image_names_both_ends_of_the_path() {
        let source = ColorProfile::new_display_p3();
        let display = ColorProfile::new_srgb();
        let transform = ColorTransform::new(&source, &display);
        let passport = Passport::new(Some(&source), &display, &transform);

        assert!(passport.converting, "a P3 image on an sRGB display is a conversion");
        assert!(passport.source.is_some(), "the file's profile went unnamed");
        assert!(passport.display.is_some(), "the display's profile went unnamed");
        assert!(passport.distance > 0.0, "a real conversion measured no distance at all");
    }

    /// An image that matches the display is converted by nothing, and the
    /// passport has to say so rather than describing an identity as work.
    #[test]
    fn a_matching_image_reports_no_conversion() {
        let profile = ColorProfile::new_srgb();
        let transform = ColorTransform::new(&profile, &profile);
        let passport = Passport::new(Some(&profile), &profile, &transform);

        assert!(!passport.converting);
        assert!(passport.distance < 0.001, "an identity moved a primary by {}", passport.distance);
        assert!(passport.summary().contains("matches the display"), "{}", passport.summary());
    }

    /// An untagged file states nothing, and the passport says that rather than
    /// naming a profile it does not have — the decision ADR 0005 records.
    #[test]
    fn an_untagged_file_says_it_states_nothing() {
        let display = ColorProfile::new_srgb();
        let passport = Passport::new(None, &display, &ColorTransform::identity());

        assert_eq!(passport.source, None);
        assert!(!passport.converting);
        assert!(
            passport.summary().contains("does not say"),
            "an untagged file was described as though it had a profile: {}",
            passport.summary(),
        );
    }

    /// The distance is a sense of scale, so spaces further apart have to
    /// measure further apart — a figure that did not order them would be
    /// decoration.
    #[test]
    fn spaces_further_apart_measure_further_apart() {
        let display = ColorProfile::new_srgb();

        let near = Passport::new(
            Some(&ColorProfile::new_display_p3()),
            &display,
            &ColorTransform::new(&ColorProfile::new_display_p3(), &display),
        );
        let far = Passport::new(
            Some(&ColorProfile::new_bt2020()),
            &display,
            &ColorTransform::new(&ColorProfile::new_bt2020(), &display),
        );

        assert!(
            far.distance > near.distance,
            "BT.2020 ({}) did not measure further from sRGB than P3 ({})",
            far.distance,
            near.distance,
        );
    }

    /// The summary is what a person reads, so it has to name the file's own
    /// space rather than describing the conversion in the abstract.
    #[test]
    fn the_summary_names_the_space_the_file_is_in() {
        let source = ColorProfile::new_display_p3();
        let display = ColorProfile::new_srgb();
        let transform = ColorTransform::new(&source, &display);
        let passport = Passport::new(Some(&source), &display, &transform);

        let summary = passport.summary();
        let named = passport.source.expect("the source was named");
        assert!(summary.contains(&named), "the summary does not name the file's space: {summary}");
        assert!(summary.contains("converted"), "the summary does not say a conversion is happening: {summary}");
    }

    /// A profile with no description of its own leaves the name empty rather
    /// than reporting whitespace or a stray terminator as a profile name.
    #[test]
    fn a_nameless_profile_is_reported_as_nameless() {
        let mut profile = ColorProfile::new_srgb();
        profile.description = Some(ProfileText::PlainString("   \0  ".to_string()));
        assert_eq!(profile_name(&profile), None, "padding was reported as a profile name");

        profile.description = None;
        assert_eq!(profile_name(&profile), None);
    }

    /// The three encodings a profile can carry its name in are all read: a
    /// viewer that handled only the modern one would leave most display
    /// profiles nameless, because the ones Windows ships are old.
    #[test]
    fn a_profile_name_is_read_in_every_encoding_it_can_carry() {
        let mut profile = ColorProfile::new_srgb();

        profile.description = Some(ProfileText::PlainString("Plain".into()));
        assert_eq!(profile_name(&profile).as_deref(), Some("Plain"));

        profile.description = Some(ProfileText::Localizable(vec![moxcms::LocalizableString {
            language: "en".into(),
            country: "US".into(),
            value: "Localised".into(),
        }]));
        assert_eq!(profile_name(&profile).as_deref(), Some("Localised"));

        profile.description = Some(ProfileText::Description(moxcms::DescriptionString {
            ascii_string: "Ascii".into(),
            unicode_language_code: 0,
            unicode_string: "Unicode".into(),
            script_code_code: 0,
            mac_string: String::new(),
        }));
        assert_eq!(profile_name(&profile).as_deref(), Some("Ascii"), "the old form's ASCII name was not read");

        // And the unicode half is the fallback when the ASCII one is empty.
        profile.description = Some(ProfileText::Description(moxcms::DescriptionString {
            ascii_string: String::new(),
            unicode_language_code: 0,
            unicode_string: "Unicode".into(),
            script_code_code: 0,
            mac_string: String::new(),
        }));
        assert_eq!(profile_name(&profile).as_deref(), Some("Unicode"));
    }

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

    /// A PQ profile's reference white must land on 1.0 after the transform.
    ///
    /// The decoded PQ curve reaches 1.0 at ten thousand nits, so the 203-nit
    /// reference white decodes to about 0.0203 — and without the scale folded
    /// into the matrix, every HDR10 image would show nearly black. The scale
    /// is checked end to end here: curve times matrix, grey in, SDR white
    /// out.
    #[test]
    fn pq_reference_white_lands_on_sdr_white() {
        let cicp = moxcms::CicpProfile {
            color_primaries: 9u8.try_into().unwrap(),
            transfer_characteristics: 16u8.try_into().unwrap(),
            matrix_coefficients: 9u8.try_into().unwrap(),
            full_range: true,
        };
        let mut profile = ColorProfile::new_srgb();
        profile.update_rgb_colorimetry_from_cicp(cicp);

        let transform = ColorTransform::new(&profile, &ColorProfile::new_srgb());
        assert!(!transform.is_identity, "a PQ image must be converted");

        // The PQ code for 203 nits, decoded through the sampled curve.
        let code = 0.5806f32;
        let position = code * (CURVE_SAMPLES - 1) as f32;
        let index = position.floor() as usize;
        let fraction = position - index as f32;
        let linear = |channel: usize| {
            let row = &transform.decode[channel * CURVE_SAMPLES..(channel + 1) * CURVE_SAMPLES];
            row[index] + (row[index + 1] - row[index]) * fraction
        };

        // Grey through the matrix: each output row sums its weights.
        for row in 0..3 {
            let out: f32 = (0..3).map(|column| transform.matrix[row][column] * linear(column)).sum();
            assert!((out - 1.0).abs() < 0.05, "PQ reference white came out at {out} on row {row}");
        }
    }

    /// A curve that is not PQ is not scaled: sRGB white stays white.
    #[test]
    fn only_pq_light_is_rescaled() {
        let transform = ColorTransform::new(&ColorProfile::new_display_p3(), &ColorProfile::new_srgb());
        // The matrix rows of a P3-to-sRGB transform sum to about 1.0 —
        // white maps to white. A stray PQ scale would blow this up 49-fold.
        for row in 0..3 {
            let sum: f32 = transform.matrix[row].iter().sum();
            assert!((sum - 1.0).abs() < 0.02, "row {row} of an SDR transform sums to {sum}");
        }
    }
}
