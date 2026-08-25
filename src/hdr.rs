//! Choosing the output signal: standard range, or high.
//!
//! The decision has two halves, and keeping them apart is what makes it
//! testable without a GPU. What the surface *can* do is a capability question
//! — `SurfaceCapabilities` answers it once and it does not change. What the
//! display is doing *right now* is a live question — the HDR toggle in Windows
//! moves it while the viewer is open, so it is asked again every frame's worth
//! of reconfiguration and the swapchain follows.
//!
//! nitid asks both, because either alone gets it wrong: configuring an HDR
//! surface on a display in SDR mode costs a wider frame buffer and buys
//! nothing, and refusing to configure one because the display is in SDR *now*
//! would leave the viewer stuck in SDR after the user turns HDR on.
//!
//! On DX12 two HDR paths are reachable, both measured on the author's machine:
//! `ExtendedSrgbLinear` on `Rgba16Float` (scRGB) and `Bt2100Pq` on
//! `Rgb10a2Unorm` (HDR10). nitid takes scRGB — see
//! `docs/adr/0013-hdr-output-goes-through-scrgb.md`.

use wgpu::{SurfaceCapabilities, SurfaceColorSpace, TextureFormat};

/// The format and colour space the surface is configured with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Output {
    pub format: TextureFormat,
    pub color_space: SurfaceColorSpace,
}

impl Output {
    /// Whether the shader must write extended-range linear light rather than
    /// sRGB-encoded values clamped to 0..1.
    pub fn is_hdr(self) -> bool {
        self.color_space.is_hdr()
    }

    /// Whether the surface applies the sRGB transfer function on write.
    ///
    /// True only for an `*Srgb` texture format; the colour space does not
    /// encode for us.
    pub fn encodes_srgb(self) -> bool {
        self.format.is_srgb()
    }
}

/// The extended-range linear pair: linear light above 1.0 drives
/// brighter-than-SDR output, which is exactly what the shader already
/// produces on its way to the sRGB encoder.
const SCRGB: Output = Output {
    format: TextureFormat::Rgba16Float,
    color_space: SurfaceColorSpace::ExtendedSrgbLinear,
};

/// Pick the output for a surface, given what it supports and how much headroom
/// the display reports.
///
/// `headroom` is `DisplayHdrInfo::tone_map_headroom`: the linear multiplier of
/// SDR white the display can drive. `None` means the platform would not say,
/// and is treated as SDR — guessing HDR on an unknown display would show a
/// picture nobody asked for.
pub fn choose(capabilities: &SurfaceCapabilities, headroom: Option<f32>) -> Output {
    if wants_hdr(headroom) && supports(capabilities, SCRGB) {
        return SCRGB;
    }
    standard(capabilities)
}

/// Whether the display has room above SDR white worth reaching for.
///
/// The threshold is deliberately above 1.0 rather than at it: a display that
/// reports exactly its SDR white as its peak has no headroom to drive, and a
/// hair over that is measurement noise, not a picture the viewer can show.
fn wants_hdr(headroom: Option<f32>) -> bool {
    matches!(headroom, Some(headroom) if headroom > HEADROOM_THRESHOLD)
}

/// How much brighter than SDR white a display must go before nitid asks for an
/// HDR surface. A tenth of a stop of headroom would not be visible and would
/// still cost the wider buffer.
const HEADROOM_THRESHOLD: f32 = 1.05;

/// Whether the surface can be configured with this format and colour space.
fn supports(capabilities: &SurfaceCapabilities, output: Output) -> bool {
    let Some(wanted) = output.color_space.to_color_spaces() else {
        // `Auto` names no flag; it is supported wherever the format is listed.
        return capabilities.formats.contains(&output.format);
    };
    capabilities.color_spaces(output.format).contains(wanted)
}

/// The standard-range output: an sRGB format so the hardware encodes on write,
/// which is both free and correctly filtered.
///
/// Falls back to the surface's preferred format when none is sRGB — a
/// configuration nitid has not met, but one where showing the picture beats
/// refusing to open a window.
fn standard(capabilities: &SurfaceCapabilities) -> Output {
    let format = capabilities
        .formats
        .iter()
        .copied()
        .find(TextureFormat::is_srgb)
        .unwrap_or_else(|| capabilities.formats.first().copied().unwrap_or(TextureFormat::Bgra8UnormSrgb));

    Output {
        format,
        color_space: SurfaceColorSpace::Auto,
    }
}

#[cfg(test)]
mod tests {
    use wgpu::SurfaceColorSpaces;

    use super::*;

    /// Capabilities shaped like the ones DX12 reports on the author's machine:
    /// four 8-bit formats in sRGB, `Rgb10a2Unorm` also in PQ, and
    /// `Rgba16Float` only in extended linear.
    fn windows_capabilities() -> SurfaceCapabilities {
        SurfaceCapabilities {
            formats: vec![
                TextureFormat::Bgra8UnormSrgb,
                TextureFormat::Rgba8UnormSrgb,
                TextureFormat::Bgra8Unorm,
                TextureFormat::Rgba8Unorm,
                TextureFormat::Rgb10a2Unorm,
                TextureFormat::Rgba16Float,
            ],
            format_capabilities: vec![
                capability(TextureFormat::Bgra8UnormSrgb, SurfaceColorSpaces::SRGB),
                capability(TextureFormat::Rgba8UnormSrgb, SurfaceColorSpaces::SRGB),
                capability(TextureFormat::Bgra8Unorm, SurfaceColorSpaces::SRGB),
                capability(TextureFormat::Rgba8Unorm, SurfaceColorSpaces::SRGB),
                capability(TextureFormat::Rgb10a2Unorm, SurfaceColorSpaces::SRGB | SurfaceColorSpaces::BT2100_PQ),
                capability(TextureFormat::Rgba16Float, SurfaceColorSpaces::EXTENDED_SRGB_LINEAR),
            ],
            ..Default::default()
        }
    }

    fn capability(format: TextureFormat, color_spaces: SurfaceColorSpaces) -> wgpu::SurfaceFormatCapabilities {
        wgpu::SurfaceFormatCapabilities { format, color_spaces }
    }

    #[test]
    fn a_display_with_headroom_gets_the_extended_linear_surface() {
        let output = choose(&windows_capabilities(), Some(7.7));

        assert_eq!(output, SCRGB);
        assert!(output.is_hdr());
        // The format is not an `*Srgb` one, so the shader must not hand it
        // sRGB-encoded values.
        assert!(!output.encodes_srgb());
    }

    #[test]
    fn a_display_in_sdr_mode_stays_on_the_srgb_surface() {
        // What the author's laptop reports with the Windows HDR toggle off:
        // the panel is capable, `tone_map_headroom` is 1.0 all the same.
        let output = choose(&windows_capabilities(), Some(1.0));

        assert!(!output.is_hdr());
        assert!(output.encodes_srgb(), "an SDR surface should let the hardware encode");
    }

    #[test]
    fn an_unknown_headroom_is_treated_as_standard_range() {
        // No platform figure is not a licence to guess: a viewer that decided
        // for itself would show HDR on a display that cannot take it.
        let output = choose(&windows_capabilities(), None);

        assert!(!output.is_hdr());
    }

    #[test]
    fn headroom_a_hair_over_one_is_not_headroom() {
        // Measurement noise, not a picture. The wider buffer would cost real
        // memory for nothing visible.
        assert!(!choose(&windows_capabilities(), Some(1.01)).is_hdr());
    }

    #[test]
    fn a_surface_without_the_extended_linear_pair_stays_standard() {
        // A GPU reporting `Rgba16Float` for `Auto` only — the historical
        // arrangement — must not be configured for HDR on the strength of the
        // format alone: the colour space is what makes it extended range.
        let capabilities = SurfaceCapabilities {
            formats: vec![TextureFormat::Bgra8UnormSrgb, TextureFormat::Rgba16Float],
            format_capabilities: vec![
                capability(TextureFormat::Bgra8UnormSrgb, SurfaceColorSpaces::SRGB),
                capability(TextureFormat::Rgba16Float, SurfaceColorSpaces::SRGB),
            ],
            ..Default::default()
        };

        let output = choose(&capabilities, Some(7.7));
        assert!(!output.is_hdr());
        assert_eq!(output.format, TextureFormat::Bgra8UnormSrgb);
    }

    #[test]
    fn a_surface_with_no_srgb_format_still_yields_something_to_draw_on() {
        let capabilities = SurfaceCapabilities {
            formats: vec![TextureFormat::Bgra8Unorm],
            format_capabilities: vec![capability(TextureFormat::Bgra8Unorm, SurfaceColorSpaces::SRGB)],
            ..Default::default()
        };

        let output = choose(&capabilities, Some(1.0));
        assert_eq!(output.format, TextureFormat::Bgra8Unorm);
        assert!(!output.encodes_srgb(), "a non-sRGB format leaves the encoding to the shader");
    }
}
