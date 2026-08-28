//! What the viewer shows besides the picture: a toolbar, a status line and a
//! key sheet.
//!
//! The chrome disappears by design — a viewer is for looking at photographs,
//! not at its own toolbar — which leaves a problem the reference layout named:
//! a function nobody can see is a function nobody finds. So the status line
//! states what is on screen, `?` lists every key there is, and the toolbar
//! comes back when the pointer reaches for it and leaves again when it does
//! not: reachable with a mouse, absent while looking.
//!
//! Nothing here polls. egui is asked to lay out a frame only when something it
//! shows has changed, and it is drawn only when the viewer was going to draw
//! anyway. The promise from v0.12.0 — a still image costs no wakeups — is not
//! this module's to spend.

use std::time::{Duration, Instant};

use crate::format::Format;
use crate::image_source::Depth;
use crate::view::FitMode;

/// How long a toast stays up before it fades.
///
/// Long enough to read six words after looking back at the screen, short
/// enough that it is gone before it becomes furniture.
const TOAST_LIFETIME: Duration = Duration::from_millis(2600);

/// How long the fade at the end of that takes.
const TOAST_FADE: Duration = Duration::from_millis(400);

/// How near the top of the window the pointer has to come for the toolbar to
/// appear, in logical points.
///
/// Wide enough that reaching for it is not a game of precision, narrow enough
/// that the toolbar does not appear over a photograph the pointer is merely
/// crossing on its way somewhere.
const TOOLBAR_REVEAL: f32 = 64.0;

/// How tall the toolbar itself is, so it can stay visible once shown: the
/// pointer moving onto a button must not be the thing that hides it.
const TOOLBAR_HEIGHT: f32 = 40.0;

/// What the status line says about the picture on screen.
///
/// Gathered by the application rather than read from the renderer: everything
/// here is a fact about the file or the framing, and the renderer knows about
/// neither.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub name: String,
    /// Position in the folder, one-based, and how many there are. `None` when
    /// there is only one image to look at.
    pub position: Option<(usize, usize)>,
    /// The image's own size in pixels, after orientation.
    pub size: Option<(u32, u32)>,
    pub format: Option<Format>,
    pub depth: Option<Depth>,
    /// What the colour transform is doing, in the words a person would use.
    pub colour: Option<String>,
    /// Zoom as the user reads it: 1.0 is 100%.
    pub scale: f32,
    pub fit: FitMode,
    /// Frame and count for an animation, plus whether it is paused.
    pub frame: Option<(usize, usize, bool)>,
    /// Whether the surface is carrying high dynamic range.
    pub hdr: bool,
}

/// What a toolbar button asks the viewer to do.
///
/// The interface names the intent and stops there: it has no way to move
/// through a folder or to reframe a picture, and giving it one would put the
/// viewer's behaviour in two places. The application does the doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Previous,
    Next,
    ZoomOut,
    ZoomIn,
    Fit,
    Actual,
    FullScreen,
    Keys,
}

/// A short-lived message: "path copied", "moved to the recycle bin".
///
/// Nothing raises one yet — the actions that will are versions away — but the
/// interface stage is where it costs nothing to build, and a notification that
/// arrives with the feature that needs it tends to arrive as a dialog instead.
struct Toast {
    text: String,
    raised: Instant,
}

impl Toast {
    /// How opaque this toast is now, or `None` once it is finished.
    fn opacity(&self, now: Instant) -> Option<f32> {
        let age = now.saturating_duration_since(self.raised);
        if age >= TOAST_LIFETIME {
            return None;
        }
        let remaining = TOAST_LIFETIME - age;
        Some(if remaining >= TOAST_FADE {
            1.0
        } else {
            remaining.as_secs_f32() / TOAST_FADE.as_secs_f32()
        })
    }
}

/// The interface's own state: what is shown, and what is asked for.
pub struct Interface {
    context: egui::Context,
    keys_shown: bool,
    /// Whether the toolbar is showing, decided by where the pointer is.
    ///
    /// Kept here rather than asked of egui each frame because it is also what
    /// decides whether a frame is laid out at all: the digest has to be able
    /// to see it change.
    toolbar_shown: bool,
    toasts: Vec<Toast>,
    /// The last thing laid out, kept so an unchanged frame can be skipped.
    last: Option<String>,
}

impl Default for Interface {
    fn default() -> Self {
        Self::new()
    }
}

impl Interface {
    pub fn new() -> Self {
        let context = egui::Context::default();
        // The viewer is dark by decision, not by system theme: the scene
        // behind a photograph stays dark so the photograph is what is lit.
        let mut visuals = egui::Visuals::dark();
        // The interface is chrome over a photograph, so it has no backdrop of
        // its own: each panel paints the strip it occupies and the rest stays
        // the picture. egui's root fill is only ever drawn where a panel is
        // not, which is exactly where the photograph should be showing.
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        context.set_visuals(visuals);
        Self {
            context,
            keys_shown: false,
            toolbar_shown: false,
            toasts: Vec::new(),
            last: None,
        }
    }

    pub fn context(&self) -> &egui::Context {
        &self.context
    }

    /// Show or hide the key sheet.
    pub fn toggle_keys(&mut self) {
        self.keys_shown = !self.keys_shown;
    }

    /// Follow the pointer, and say whether the toolbar's visibility changed.
    ///
    /// The toolbar appears when the pointer comes near the top of the window
    /// and stays while it is over the toolbar itself — otherwise moving onto a
    /// button would take the button away. A pointer that has left the window
    /// altogether is `None`, which hides it.
    ///
    /// The band it hides at is deeper than the one it appears at, so a pointer
    /// resting exactly on the boundary does not flicker the toolbar on and off
    /// with every pixel of movement.
    pub fn follow_pointer(&mut self, pointer: Option<(f32, f32)>) -> bool {
        let shown = match pointer {
            Some((_, y)) if self.toolbar_shown => y <= TOOLBAR_REVEAL.max(TOOLBAR_HEIGHT),
            Some((_, y)) => y <= TOOLBAR_REVEAL,
            None => false,
        };
        let changed = shown != self.toolbar_shown;
        self.toolbar_shown = shown;
        changed
    }

    /// Raise a message.
    ///
    /// The caller redraws: a toast is always raised in answer to something
    /// that was going to ask for a frame anyway.
    pub fn toast(&mut self, text: impl Into<String>, now: Instant) {
        self.toasts.push(Toast {
            text: text.into(),
            raised: now,
        });
    }

    /// Drop toasts that have finished, and say when the next one will need a
    /// frame.
    ///
    /// This is the only thing here that asks to be woken, and only while a
    /// toast is actually up: the fade has to be drawn to be seen.
    pub fn tick(&mut self, now: Instant) -> Option<Instant> {
        self.toasts.retain(|toast| toast.opacity(now).is_some());
        self.toasts.iter().map(|toast| toast.raised + TOAST_LIFETIME).min()
    }

    /// Lay out one frame, and report what was asked of it.
    ///
    /// `raw` comes from `egui-winit` and carries the pointer, the keys and the
    /// screen rectangle. The `Action` is whatever button was pressed this
    /// frame, and it is the interface's whole say in what the viewer does.
    pub fn layout(&mut self, raw: egui::RawInput, status: &Status, now: Instant) -> (egui::FullOutput, Option<Action>) {
        let keys_shown = self.keys_shown;
        let toolbar_shown = self.toolbar_shown;
        let toasts: Vec<(String, f32)> = self
            .toasts
            .iter()
            .filter_map(|toast| toast.opacity(now).map(|opacity| (toast.text.clone(), opacity)))
            .collect();

        let mut action = None;
        // `run_ui` hands over the root `Ui` rather than the context: in egui
        // 0.36 panels are shown inside a `Ui`, and this is the one they sit in.
        let output = self.context.clone().run_ui(raw, |ui| {
            if toolbar_shown {
                action = toolbar(ui, status);
            }
            status_line(ui, status);
            if keys_shown {
                key_sheet(ui);
            }
            toast_stack(ui, &toasts);
        });
        (output, action)
    }

    /// A one-line summary of what is being shown, used to tell whether the
    /// interface would draw anything different from last time.
    pub fn digest(&self, status: &Status) -> String {
        format!(
            "{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:.3}|{:?}|{:?}|{}|{}|{}|{}",
            status.name,
            status.position,
            status.size,
            status.format,
            status.depth,
            status.colour,
            status.scale,
            status.fit,
            status.frame,
            status.hdr,
            self.keys_shown,
            self.toolbar_shown,
            self.toasts.len(),
        )
    }

    /// Whether the interface would draw something different from last time.
    ///
    /// A frame that would look identical is not laid out at all. That is what
    /// keeps a still picture free: the loop already wakes for real input, and
    /// this stops it from also redrawing for nothing.
    pub fn changed(&mut self, status: &Status) -> bool {
        let digest = self.digest(status);
        let changed = self.last.as_deref() != Some(digest.as_str());
        if changed {
            self.last = Some(digest);
        }
        changed
    }
}

/// The strip along the top: the things you would reach for with a mouse.
///
/// It carries nothing that is not also a key, and every button says its key in
/// its tooltip — the toolbar is a way in for a hand on the mouse, not a second
/// set of features to keep in step with the first.
///
/// A button is disabled rather than hidden when it would do nothing: a strip
/// whose contents move about as folders change is harder to aim at than one
/// that is always the same shape.
fn toolbar(ui: &mut egui::Ui, status: &Status) -> Option<Action> {
    let mut action = None;
    let alone = status.position.is_none_or(|(_, count)| count <= 1);
    let showing = status.size.is_some();

    egui::Panel::top("toolbar")
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(14, 16, 20, 220))
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut button = |ui: &mut egui::Ui, label: &str, hint: &str, enabled: bool, wanted: Action| {
                    if ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(hint).clicked() {
                        action = Some(wanted);
                    }
                };

                button(ui, "◀", "Previous image  (←)", !alone, Action::Previous);
                button(ui, "▶", "Next image  (→)", !alone, Action::Next);
                separator(ui);
                button(ui, "−", "Zoom out  (-)", showing, Action::ZoomOut);
                button(ui, "+", "Zoom in  (+)", showing, Action::ZoomIn);
                button(ui, "Fit", "Fit to window  (0)", showing, Action::Fit);
                button(ui, "1:1", "Actual size  (1)", showing, Action::Actual);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    button(ui, "?", "Keys  (?)", true, Action::Keys);
                    button(ui, "⛶", "Full screen  (F11)", true, Action::FullScreen);
                });
            });
        });

    action
}

/// The bar along the bottom: what this picture is.
fn status_line(ui: &mut egui::Ui, status: &Status) {
    egui::Panel::bottom("status")
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(14, 16, 20, 220))
                .inner_margin(egui::Margin::symmetric(10, 5)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&status.name).strong());

                if let Some((position, count)) = status.position {
                    separator(ui);
                    ui.label(format!("{position} of {count}"));
                }

                if let Some((width, height)) = status.size {
                    separator(ui);
                    ui.label(monospace(format!("{width}×{height}")));
                }

                if let Some(format) = status.format {
                    separator(ui);
                    ui.label(format.name());
                }

                if let Some(depth) = status.depth {
                    separator(ui);
                    ui.label(monospace(match depth {
                        Depth::Eight => "8-bit",
                        Depth::Sixteen => "16-bit",
                    }));
                }

                if let Some(colour) = &status.colour {
                    separator(ui);
                    ui.label(colour);
                }

                if status.hdr {
                    separator(ui);
                    ui.label(egui::RichText::new("HDR").strong());
                }

                if let Some((frame, count, paused)) = status.frame {
                    separator(ui);
                    ui.label(monospace(format!("frame {frame}/{count}")));
                    if paused {
                        separator(ui);
                        ui.label("paused");
                    }
                }

                // Zoom sits at the right-hand end, where it does not move when
                // the file name changes length.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(monospace(format!("{:.0}%", status.scale * 100.0)));
                    if status.fit != FitMode::Free {
                        separator(ui);
                        ui.label(match status.fit {
                            FitMode::Fit => "fit",
                            FitMode::Actual => "actual",
                            FitMode::Free => "",
                        });
                    }
                });
            });
        });
}

/// Every key there is, because the chrome does not advertise them.
fn key_sheet(ui: &mut egui::Ui) {
    egui::Window::new("Keys")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            egui::Grid::new("keys").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                for (key, action) in KEYS {
                    ui.label(monospace(*key));
                    ui.label(*action);
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.label(egui::RichText::new(TOOLBAR_HINT).weak());
        });
}

/// The keys, in the order a person meets them.
///
/// Kept beside the handler it describes rather than in the documentation,
/// where the two drift apart: a test holds this list against the keys the
/// viewer actually answers.
pub const KEYS: &[(&str, &str)] = &[
    ("← →", "previous / next image"),
    ("Home End", "first / last image"),
    ("Space", "pause an animation, or the next image"),
    ("Wheel", "zoom around the cursor"),
    ("Drag", "pan"),
    ("Middle click", "toggle fit and 100%"),
    ("0 1", "fit to window / actual size"),
    ("+ -", "zoom in / out"),
    ("F11", "full screen"),
    ("?", "this list"),
    ("Esc", "quit"),
];

/// What the toolbar is, said where a person looking for it would read it.
const TOOLBAR_HINT: &str = "The toolbar appears when the pointer reaches the top of the window.";

/// Messages, stacked above the status line.
fn toast_stack(ui: &mut egui::Ui, toasts: &[(String, f32)]) {
    if toasts.is_empty() {
        return;
    }

    egui::Area::new("toasts".into())
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -52.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            for (text, opacity) in toasts {
                let alpha = (opacity * 235.0) as u8;
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_unmultiplied(24, 27, 33, alpha))
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .corner_radius(6)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(text).color(egui::Color32::from_rgba_unmultiplied(235, 238, 245, alpha)));
                    });
            }
        });
}

fn separator(ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("·").weak());
}

fn monospace(text: impl Into<String>) -> egui::RichText {
    // Monospace for anything measurable, so digits do not jitter as they
    // change — the reference layout's rule.
    egui::RichText::new(text).monospace()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Status {
        Status {
            name: "photo.jpg".into(),
            position: Some((3, 12)),
            size: Some((4000, 3000)),
            format: Some(Format::Jpeg),
            depth: Some(Depth::Eight),
            colour: Some("Display P3".into()),
            scale: 0.5,
            fit: FitMode::Fit,
            frame: None,
            hdr: false,
        }
    }

    /// The interface must not ask for a frame when nothing it shows moved.
    /// This is what keeps a still picture costing nothing.
    #[test]
    fn an_unchanged_status_asks_for_no_redraw() {
        let mut interface = Interface::new();
        assert!(interface.changed(&status()), "the first frame is always a change");
        assert!(!interface.changed(&status()), "the same status asked for a redraw");
        assert!(!interface.changed(&status()));
    }

    #[test]
    fn every_part_of_the_status_is_noticed_when_it_changes() {
        let mut interface = Interface::new();
        interface.changed(&status());

        let mut moved = status();
        moved.scale = 0.75;
        assert!(interface.changed(&moved), "a zoom change went unnoticed");

        let mut renamed = moved.clone();
        renamed.name = "other.png".into();
        assert!(interface.changed(&renamed), "a file change went unnoticed");

        let mut animated = renamed.clone();
        animated.frame = Some((2, 30, false));
        assert!(interface.changed(&animated), "an animation frame went unnoticed");

        let mut hdr = animated.clone();
        hdr.hdr = true;
        assert!(interface.changed(&hdr), "the surface changing went unnoticed");
    }

    /// The toolbar appears when the pointer reaches the top of the window and
    /// goes away when it does not — that is the whole of its behaviour, and
    /// the reference layout's rule that the chrome disappears rests on it.
    #[test]
    fn the_toolbar_follows_the_pointer_to_the_top_of_the_window() {
        let mut interface = Interface::new();
        assert!(!interface.toolbar_shown, "the toolbar was up before the pointer moved");

        // Down over the picture: nothing.
        assert!(!interface.follow_pointer(Some((400.0, 500.0))));
        assert!(!interface.toolbar_shown);

        // Up at the top: it appears, and that is a change worth a frame.
        assert!(interface.follow_pointer(Some((400.0, 10.0))), "reaching the top did not ask for a frame");
        assert!(interface.toolbar_shown);

        // Still up there: no second frame for the same state.
        assert!(!interface.follow_pointer(Some((420.0, 12.0))), "the toolbar asked for a frame for nothing");

        // Back down over the picture: it goes.
        assert!(interface.follow_pointer(Some((400.0, 500.0))));
        assert!(!interface.toolbar_shown);
    }

    /// Moving onto a button must not be what takes the button away. The band
    /// the toolbar hides at is deeper than the one it appears at, so a pointer
    /// resting between the two does not flicker it.
    #[test]
    fn the_toolbar_stays_up_while_the_pointer_is_on_it() {
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        assert!(interface.toolbar_shown);

        // Deeper than the reveal band but still within the toolbar itself.
        let inside = TOOLBAR_REVEAL.max(TOOLBAR_HEIGHT);
        assert!(!interface.follow_pointer(Some((400.0, inside))), "the toolbar hid while the pointer was on it");
        assert!(interface.toolbar_shown);

        // A pixel past it, and it goes.
        assert!(interface.follow_pointer(Some((400.0, inside + 1.0))));
        assert!(!interface.toolbar_shown);
    }

    /// A pointer that has left the window is not reaching for anything.
    #[test]
    fn the_toolbar_goes_when_the_pointer_leaves_the_window() {
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        assert!(interface.toolbar_shown);

        assert!(interface.follow_pointer(None), "the pointer leaving did not ask for a frame");
        assert!(!interface.toolbar_shown, "the toolbar stayed up with the pointer gone");
    }

    /// The toolbar is chrome, so its coming and going has to be drawn.
    #[test]
    fn the_toolbar_appearing_is_a_change() {
        let mut interface = Interface::new();
        interface.changed(&status());
        interface.follow_pointer(Some((400.0, 10.0)));
        assert!(interface.changed(&status()), "the toolbar appearing went unnoticed");
        interface.follow_pointer(None);
        assert!(interface.changed(&status()), "the toolbar going away went unnoticed");
    }

    #[test]
    fn showing_the_key_sheet_is_a_change() {
        let mut interface = Interface::new();
        interface.changed(&status());
        interface.toggle_keys();
        assert!(interface.changed(&status()), "opening the key sheet went unnoticed");
        interface.toggle_keys();
        assert!(interface.changed(&status()), "closing the key sheet went unnoticed");
    }

    #[test]
    fn a_toast_fades_and_then_finishes() {
        let now = Instant::now();
        let toast = Toast {
            text: "path copied".into(),
            raised: now,
        };

        assert_eq!(toast.opacity(now), Some(1.0));
        // Still fully opaque well before the fade begins.
        assert_eq!(toast.opacity(now + TOAST_LIFETIME - TOAST_FADE), Some(1.0));

        // Half way through the fade.
        let half = toast.opacity(now + TOAST_LIFETIME - TOAST_FADE / 2).expect("still up");
        assert!((half - 0.5).abs() < 0.05, "half way through the fade the toast was {half}");

        // Finished.
        assert_eq!(toast.opacity(now + TOAST_LIFETIME), None);
        assert_eq!(toast.opacity(now + TOAST_LIFETIME * 2), None);
    }

    /// A toast has to be drawn as it fades, so it is the one thing here that
    /// asks the loop to wake. When none is up, it asks for nothing.
    #[test]
    fn only_a_live_toast_asks_for_a_wakeup() {
        let mut interface = Interface::new();
        let now = Instant::now();

        assert_eq!(interface.tick(now), None, "an idle interface asked to be woken");

        interface.toast("path copied", now);
        let due = interface.tick(now).expect("a live toast asks for a frame");
        assert!(due > now, "the deadline is in the past, so the loop would spin");
        assert!(due <= now + TOAST_LIFETIME);

        // Once it has expired it is dropped, and nothing asks again.
        assert_eq!(interface.tick(now + TOAST_LIFETIME), None, "a finished toast still asked to be woken");
    }

    /// Lay out one frame with the pointer where it is, optionally clicking,
    /// and report what the interface asked for.
    ///
    /// This goes through egui's real layout and hit testing: a test that only
    /// read `Action` back from a function I wrote would prove that the enum
    /// exists, not that a button can be pressed.
    fn press(interface: &mut Interface, at: egui::Pos2, click: bool) -> Option<Action> {
        press_showing(interface, at, click, &status())
    }

    /// The same, with the status the toolbar is laid out against spelled out:
    /// what a button does depends on what is being shown.
    fn press_showing(interface: &mut Interface, at: egui::Pos2, click: bool, status: &Status) -> Option<Action> {
        let mut events = vec![egui::Event::PointerMoved(at)];
        if click {
            events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0))),
            events,
            ..Default::default()
        };
        let (mut output, action) = interface.layout(raw, status, Instant::now());
        // egui insists its texture deltas are acknowledged rather than
        // dropped, the same as the renderer does with them for real.
        output.textures_delta.clear();
        action
    }

    /// Where the first toolbar button ends up, found by walking across the
    /// strip rather than assuming a pixel: egui decides the widths.
    fn find_button(interface: &mut Interface, wanted: Action) -> egui::Pos2 {
        for x in 8..600 {
            let at = egui::pos2(x as f32, TOOLBAR_HEIGHT / 2.0);
            // egui needs the frame before the click to have the widget in the
            // same place, so each probe is a move followed by a press.
            press(interface, at, false);
            if press(interface, at, true) == Some(wanted) {
                return at;
            }
        }
        panic!("no button on the toolbar asked for {wanted:?}");
    }

    /// The measurement the toolbar exists for: a button can be pressed, and
    /// pressing it asks for the thing it is labelled with.
    #[test]
    fn a_toolbar_button_can_actually_be_pressed() {
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));

        let at = find_button(&mut interface, Action::Next);

        // Moving over it without pressing asks for nothing.
        assert_eq!(press(&mut interface, at, false), None, "hovering a button acted on it");
        // And pressing it asks for exactly one thing.
        assert_eq!(press(&mut interface, at, true), Some(Action::Next));
    }

    /// A hidden toolbar has no buttons to hit, whatever the pointer does.
    #[test]
    fn a_hidden_toolbar_cannot_be_pressed() {
        let mut interface = Interface::new();
        // The pointer is at the top, but `follow_pointer` was never told, so
        // the toolbar is not laid out at all.
        let at = egui::pos2(20.0, TOOLBAR_HEIGHT / 2.0);
        press(&mut interface, at, false);
        assert_eq!(press(&mut interface, at, true), None, "a hidden toolbar answered a click");
    }

    /// Every action the toolbar offers must be reachable by pressing it — a
    /// button that lays out under another one, or off the end of the strip,
    /// is a feature nobody has.
    #[test]
    fn every_toolbar_action_is_reachable() {
        for wanted in [Action::Previous, Action::Next, Action::ZoomOut, Action::ZoomIn, Action::Fit, Action::Actual] {
            let mut interface = Interface::new();
            interface.follow_pointer(Some((400.0, 10.0)));
            find_button(&mut interface, wanted);
        }
    }

    /// With one file open there is nowhere to step to, and the buttons that
    /// would say otherwise are dead. A button that looks live and does nothing
    /// is worse than no button.
    #[test]
    fn stepping_is_dead_when_there_is_only_one_image() {
        let mut alone = status();
        alone.position = None;

        // Find where the step buttons are while a folder is open...
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        let next = find_button(&mut interface, Action::Next);
        let previous = find_button(&mut interface, Action::Previous);

        // ...then press the same places with a single file open.
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        for at in [next, previous] {
            press_showing(&mut interface, at, false, &alone);
            assert_eq!(
                press_showing(&mut interface, at, true, &alone),
                None,
                "a step button answered with only one image open",
            );
        }

        // The rest of the strip is still live, or this would pass by drawing
        // no toolbar at all.
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        let fit = find_button(&mut interface, Action::Fit);
        press_showing(&mut interface, fit, false, &alone);
        assert_eq!(
            press_showing(&mut interface, fit, true, &alone),
            Some(Action::Fit),
            "the whole toolbar went dead, not just the step buttons",
        );
    }

    #[test]
    fn the_key_sheet_lists_the_keys_that_exist() {
        // A cheap guard against the sheet and the handler drifting apart: the
        // characters the viewer answers must all appear.
        let listed: String = KEYS.iter().map(|(key, _)| *key).collect::<Vec<_>>().join(" ");
        for key in ["←", "→", "Home", "End", "Space", "F11", "Esc", "0", "1", "+", "-", "?"] {
            assert!(listed.contains(key), "the key sheet does not mention {key}");
        }
    }
}
