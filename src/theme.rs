//! How the interface looks: the lacodda line's tokens, and the few colours that
//! are nitid's own for a reason.
//!
//! The colours of the chrome — the bars, the panels, the dialogs, the toasts —
//! are dowel's, resolved for nitid's accent and generated into `dowel` by
//! `build.rs` from `assets/dowel/`. Nothing in `interface.rs` names a colour by
//! its numbers: it asks this module, and a test holds it to that. That is what
//! makes the next dowel release a file copy rather than a hunt through five
//! thousand lines for the one grey that was typed by hand.
//!
//! Two kinds of colour are deliberately *not* tokens, and they live here too so
//! that the exception has one address:
//!
//! - **Marks drawn on the photograph** — the crop frame, the frame on the
//!   minimap, the eyedropper's centre. They have to read against any picture,
//!   light or dark, so they are black and white paired, the way a camera's
//!   focus frame is; a themed colour would vanish into a photograph of its own
//!   hue.
//! - **Colours that belong to the data** — the three channels of the histogram
//!   are red, green and blue because that is what they measure, and a product
//!   accent there would be a lie about the file.
//!
//! The scene behind the picture is not here at all: it is the renderer's
//! (`gpu::BACKGROUND`), a neutral grey on purpose. A tint of the accent under a
//! photograph would shift how its colours are judged, and honest colour is
//! the one promise this product makes before any other.

use egui::{Color32, CornerRadius, Stroke};

/// The tokens, generated from the dowel release in `assets/dowel/`.
///
/// The whole vocabulary, not the part in use today: a step nitid does not draw
/// yet is still dowel's, and trimming the generator to match the interface
/// would make the next screen a change to the build script.
#[allow(dead_code)]
pub mod dowel {
    include!(concat!(env!("OUT_DIR"), "/dowel.rs"));
}

pub use dowel::Palette;

/// The palette the interface is drawn in right now.
///
/// Read from the visuals rather than passed about: egui already knows which
/// theme it is in (it follows the system through `egui-winit`), so a second
/// flag carried beside it could only disagree with it.
pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &dowel::DARK } else { &dowel::LIGHT }
}

/// How opaque the chrome is where it lies over the photograph.
///
/// dowel's `raise` is opaque because a card on a web page lies on the page.
/// nitid's panels lie on a picture, and a little of the picture showing
/// through is what keeps the chrome reading as "over this" rather than "instead
/// of this". The value is the one the viewer shipped with; dowel has no token
/// for a surface over content yet — lyrid made the same one by hand, and this
/// is the second, which is the line's rule for when it becomes a token.
const OVER_PICTURE: u8 = 230;

/// A surface of the chrome, as it is laid over the photograph.
pub fn over_picture(colour: Color32) -> Color32 {
    let [r, g, b, _] = colour.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, OVER_PICTURE)
}

/// The fill of a bar or a panel: dowel's raised surface, over the picture.
pub fn panel(ui: &egui::Ui) -> Color32 {
    over_picture(palette(ui).raise)
}

/// The corner of a floating panel: dowel's `lg`, the radius of a card.
pub const PANEL_RADIUS: CornerRadius = CornerRadius::same(dowel::radius::LG);

/// A colour with its alpha scaled, for something fading in or out.
pub fn faded(colour: Color32, opacity: f32) -> Color32 {
    colour.gamma_multiply(opacity.clamp(0.0, 1.0))
}

/// The light half of a mark on the photograph.
pub const MARK: Color32 = Color32::WHITE;

/// The dark half, drawn around the light one so the pair reads on any pixel.
pub const MARK_EDGE: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 160);

/// A guide inside a mark: the thirds of the crop frame. Present, not loud.
pub const MARK_GUIDE: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 90);

/// What dims the part of the picture a mark says is not chosen.
pub const SHADE: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 132);

/// An image drawn as it is: the tint egui multiplies a texture by, left alone.
pub const UNTINTED: Color32 = Color32::WHITE;

/// A pixel of the picture, shown as itself: a swatch or a cell of the
/// eyedropper's neighbourhood. The file's colour, not the interface's.
pub fn pixel([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// How bright a histogram channel is where it is present, and where it is
/// not. The floor is not zero: a band with one channel in it still has to read
/// as a column against the plot's ground.
const CHANNEL_ON: u8 = 235;
const CHANNEL_OFF: u8 = 30;

/// The colour of one band of a histogram column: the channels present in it,
/// added.
///
/// This is what stops the plot from being a picture of whichever channel was
/// painted last. All three present is white — a neutral picture reads as one
/// grey shape — and any two make the secondary between them, so a cast shows
/// as the colour of the channels that are *missing* from a band.
pub fn channels(lit: [bool; 3]) -> Color32 {
    let level = |on: bool| if on { CHANNEL_ON } else { CHANNEL_OFF };
    Color32::from_rgb(level(lit[0]), level(lit[1]), level(lit[2]))
}

/// The ground a histogram is plotted on: dark in both themes, because the
/// channels are light colours added together and need the dark to add on.
pub const PLOT_GROUND: Color32 = Color32::from_rgba_unmultiplied_const(0, 0, 0, 140);

/// The luminance line over the channels.
pub const LUMA: Color32 = Color32::from_rgba_unmultiplied_const(240, 240, 245, 210);

/// egui's visuals, made from one theme of the palette.
///
/// Built on egui's own dark or light visuals rather than from nothing, so a
/// field egui adds in a later release starts from a sensible value; every
/// colour and radius a person sees is then set from the tokens.
pub fn visuals(palette: &Palette, dark: bool) -> egui::Visuals {
    let mut visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let control = CornerRadius::same(dowel::radius::MD);

    visuals.hyperlink_color = palette.accent;
    visuals.warn_fg_color = palette.warn;
    visuals.error_fg_color = palette.bad;
    visuals.weak_text_color = Some(palette.dim);

    // The interface is chrome over a photograph, so it has no backdrop of its
    // own: each panel paints the strip it occupies and the rest stays the
    // picture. egui's root fill is only ever drawn where a panel is not, which
    // is exactly where the photograph should be showing.
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = over_picture(palette.raise);
    visuals.window_stroke = Stroke::new(1.0, palette.line_2);
    visuals.window_corner_radius = PANEL_RADIUS;
    visuals.menu_corner_radius = control;
    visuals.extreme_bg_color = palette.bg;
    visuals.text_edit_bg_color = Some(palette.bg);
    visuals.faint_bg_color = palette.soft;
    visuals.code_bg_color = palette.soft;

    visuals.selection.bg_fill = palette.accent_soft;
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = palette.raise;
    widgets.noninteractive.weak_bg_fill = palette.raise;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.line);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);

    widgets.inactive.bg_fill = palette.soft;
    widgets.inactive.weak_bg_fill = palette.soft;
    widgets.inactive.bg_stroke = Stroke::NONE;
    widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);

    widgets.hovered.bg_fill = palette.line_2;
    widgets.hovered.weak_bg_fill = palette.line_2;
    widgets.hovered.bg_stroke = Stroke::new(1.0, palette.line_2);
    widgets.hovered.fg_stroke = Stroke::new(1.5, palette.text);

    widgets.active.bg_fill = palette.accent_soft;
    widgets.active.weak_bg_fill = palette.accent_soft;
    widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    widgets.active.fg_stroke = Stroke::new(1.5, palette.text);

    widgets.open = widgets.hovered;

    for state in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = control;
    }

    visuals
}

/// The type steps egui draws text in, taken from dowel's scale.
///
/// Body text is dowel's `sm` rather than `base`: a viewer's chrome is read at
/// a glance over a photograph, the way a status bar is, not as a page.
pub fn text_styles() -> std::collections::BTreeMap<egui::TextStyle, egui::FontId> {
    use egui::{FontFamily, FontId, TextStyle};
    [
        (TextStyle::Small, FontId::new(dowel::text::XS.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(dowel::text::SM.0, FontFamily::Proportional)),
        (TextStyle::Button, FontId::new(dowel::text::SM.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(dowel::text::SM.0, FontFamily::Monospace)),
        (TextStyle::Heading, FontId::new(dowel::text::LG.0, FontFamily::Proportional)),
    ]
    .into()
}

/// Both themes of the palette, handed to egui, which picks between them by the
/// system's setting and switches when it changes.
pub fn apply(context: &egui::Context) {
    context.set_visuals_of(egui::Theme::Dark, visuals(&dowel::DARK, true));
    context.set_visuals_of(egui::Theme::Light, visuals(&dowel::LIGHT, false));
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        context.style_mut_of(theme, |style| style.text_styles = text_styles());
    }
    context.options_mut(|options| options.theme_preference = egui::ThemePreference::System);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_palette_is_the_one_dowel_resolved_for_nitid() {
        // The accent, as the brand-line registry gives it: if the generator
        // had read another product's file, or the dark theme's values had gone
        // into the light constant, this is the first number that would move.
        assert_eq!(dowel::DARK.accent, Color32::from_rgb(0x3f, 0xa9, 0xd9));
        assert_ne!(dowel::LIGHT.accent, dowel::DARK.accent);
        assert_eq!(dowel::LIGHT.raise, Color32::from_rgb(0xff, 0xff, 0xff));
    }

    #[test]
    fn a_translucent_token_keeps_its_alpha() {
        // `soft` is white at a few percent on dark. Made opaque by the
        // generator, every hovered button would be a white slab.
        let [_, _, _, alpha] = dowel::DARK.soft.to_srgba_unmultiplied();
        assert!(alpha > 0 && alpha < 32, "soft came through with alpha {alpha}");
    }

    #[test]
    fn each_theme_is_drawn_in_its_own_palette() {
        let dark = visuals(&dowel::DARK, true);
        let light = visuals(&dowel::LIGHT, false);
        assert!(dark.dark_mode && !light.dark_mode);
        assert_eq!(dark.widgets.noninteractive.fg_stroke.color, dowel::DARK.text);
        assert_eq!(light.widgets.noninteractive.fg_stroke.color, dowel::LIGHT.text);
        assert_eq!(dark.selection.stroke.color, dowel::DARK.accent);
    }

    #[test]
    fn controls_take_the_control_radius() {
        let visuals = visuals(&dowel::DARK, true);
        assert_eq!(visuals.widgets.inactive.corner_radius, CornerRadius::same(dowel::radius::MD));
        assert_eq!(dowel::radius::MD, 9, "dowel's control radius moved; look at the toolbar before accepting it");
    }
}
