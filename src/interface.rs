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

use std::path::PathBuf;

use crate::format::Format;
use crate::histogram::{BUCKETS, Histogram};
use crate::image_source::Depth;
use crate::metadata::Metadata;
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
    /// Whether the framing is held across a step to the next image.
    pub locked: bool,
    /// What shows through a transparent pixel, when it is not the viewer's
    /// own scene. `None` for the default, which needs no announcing.
    pub backdrop: Option<&'static str>,
    /// What the file says about itself, for the Info panel.
    pub metadata: Metadata,
    /// What the viewer knows without asking the file: the path and the bytes
    /// on disk. Shown in the panel beside the camera's own account.
    pub path: Option<PathBuf>,
    pub file_size: Option<u64>,
    /// What tones the picture is made of, once it has been counted.
    ///
    /// `None` while the count is still running, which is what the panel shows
    /// as "counting" rather than as an empty pair of axes.
    pub histogram: Option<Histogram>,
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
    TurnLeft,
    TurnRight,
    Backdrop,
    Lock,
    Info,
    Histogram,
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
    /// Whether the Info panel is showing.
    info_shown: bool,
    /// Whether the histogram is showing.
    histogram_shown: bool,
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
            info_shown: false,
            histogram_shown: false,
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

    /// Show or hide the Info panel.
    pub fn toggle_info(&mut self) {
        self.info_shown = !self.info_shown;
    }

    /// Show or hide the histogram.
    pub fn toggle_histogram(&mut self) {
        self.histogram_shown = !self.histogram_shown;
    }

    /// Whether the histogram is showing.
    ///
    /// Asked by the application before it starts a count: the tones are
    /// measured only while something is there to read them.
    pub fn histogram_shown(&self) -> bool {
        self.histogram_shown
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
        let info_shown = self.info_shown;
        let histogram_shown = self.histogram_shown;
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
                action = toolbar(ui, status, info_shown, histogram_shown);
            }
            status_line(ui, status);
            if info_shown {
                info_panel(ui, status);
            }
            if histogram_shown {
                histogram_panel(ui, status);
            }
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
            "{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:.3}|{:?}|{:?}|{}|{}|{:?}|{}|{}|{}|{}",
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
            status.locked,
            status.backdrop,
            self.keys_shown,
            self.info_shown,
            self.toolbar_shown,
            self.toasts.len(),
        ) + &format!(
            "|{}|{:?}",
            self.histogram_shown,
            // The counted total stands in for the whole shape: a histogram
            // that arrives, or one replaced by the next picture's, changes it.
            // Without this the panel would open empty and stay empty, because
            // nothing else in the digest moves when a count lands.
            status.histogram.as_ref().map(|histogram| histogram.counted),
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
fn toolbar(ui: &mut egui::Ui, status: &Status, info_shown: bool, histogram_shown: bool) -> Option<Action> {
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
                action = button(ui, "◀", "Previous image  (←)", !alone, Action::Previous)
                    .or(button(ui, "▶", "Next image  (→)", !alone, Action::Next))
                    .or_else(|| {
                        separator(ui);
                        None
                    })
                    .or(button(ui, "−", "Zoom out  (-)", showing, Action::ZoomOut))
                    .or(button(ui, "+", "Zoom in  (+)", showing, Action::ZoomIn))
                    .or(button(ui, "Fit", "Fit to window  (0)", showing, Action::Fit))
                    .or(button(ui, "1:1", "Actual size  (1)", showing, Action::Actual))
                    .or_else(|| {
                        separator(ui);
                        None
                    })
                    .or(button(ui, "↺", "Turn anticlockwise  (Shift+R)", showing, Action::TurnLeft))
                    .or(button(ui, "↻", "Turn clockwise  (R)", showing, Action::TurnRight));

                // The right-hand end, laid out from the right. The two that
                // carry a state show which one they are in rather than only
                // what they would do.
                let from_the_right = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        button(ui, "?", "Keys  (?)", true, Action::Keys)
                            .or(button(ui, "⛶", "Full screen  (F11)", true, Action::FullScreen))
                            .or(toggle(ui, "Info", "What the file says about itself  (I)", showing, info_shown, Action::Info))
                            .or(toggle(
                                ui,
                                "Tones",
                                "What tones the picture is made of  (H)",
                                showing,
                                histogram_shown,
                                Action::Histogram,
                            ))
                            .or_else(|| {
                                separator(ui);
                                None
                            })
                            .or(toggle(ui, "Lock", "Hold the framing across a step  (L)", !alone, status.locked, Action::Lock))
                            .or(toggle(
                                ui,
                                status.backdrop.unwrap_or("scene"),
                                "What shows through transparency  (B)",
                                true,
                                status.backdrop.is_some(),
                                Action::Backdrop,
                            ))
                    })
                    .inner;
                action = action.or(from_the_right);
            });
        });

    action
}

/// One toolbar button, reporting what it asks for when pressed.
fn button(ui: &mut egui::Ui, label: &str, hint: &str, enabled: bool, wanted: Action) -> Option<Action> {
    ui.add_enabled(enabled, egui::Button::new(label))
        .on_hover_text(hint)
        .clicked()
        .then_some(wanted)
}

/// A button that carries a state, and shows which one it is in.
fn toggle(ui: &mut egui::Ui, label: &str, hint: &str, enabled: bool, on: bool, wanted: Action) -> Option<Action> {
    ui.add_enabled(enabled, egui::Button::new(label).selected(on))
        .on_hover_text(hint)
        .clicked()
        .then_some(wanted)
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

                // Only when it is not the viewer's own scene: the default
                // needs no name, because it is what the viewer always is.
                if let Some(backdrop) = status.backdrop {
                    separator(ui);
                    ui.label(backdrop).on_hover_text("What shows through a transparent pixel  (B)");
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
                    // Beside the zoom, because that is what it holds.
                    if status.locked {
                        separator(ui);
                        ui.label(egui::RichText::new("locked").strong())
                            .on_hover_text("The framing is held across a step to the next image  (L)");
                    }
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

/// What the file says about itself, down the right-hand edge.
///
/// An overlay rather than a column that pushes the picture aside: the chrome
/// disappears by design, and a panel that reflowed the framing every time it
/// opened would move the thing being looked at.
///
/// Every row copies its value when clicked. A lens name, a shutter speed or a
/// coordinate is nearly always wanted somewhere else — a caption, a search, a
/// map — and reading it off the screen to type it back in is the part that
/// wastes the panel.
fn info_panel(ui: &mut egui::Ui, status: &Status) {
    egui::Panel::right("info")
        .exact_size(300.0)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(14, 16, 20, 235))
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(egui::RichText::new("Info").strong());
                ui.add_space(6.0);

                // What the viewer knows without asking the file. Always
                // present, so a screenshot with no EXIF still says something.
                if let Some((width, height)) = status.size {
                    row(ui, "Size", &format!("{width} × {height}"), true);
                }
                if let Some(format) = status.format {
                    row(ui, "Format", format.name(), false);
                }
                if let Some(depth) = status.depth {
                    row(
                        ui,
                        "Depth",
                        match depth {
                            Depth::Eight => "8-bit",
                            Depth::Sixteen => "16-bit",
                        },
                        true,
                    );
                }
                if let Some(colour) = &status.colour {
                    row(ui, "Colour", colour, false);
                }
                if let Some(bytes) = status.file_size {
                    row(ui, "File", &file_size(bytes), true);
                }
                if let Some(path) = &status.path {
                    row(ui, "Path", &path.display().to_string(), false);
                }

                // What the camera wrote. Absent for every screenshot and most
                // PNGs, which is why the section only appears when there is
                // something in it.
                if !status.metadata.camera.is_empty() {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Camera").strong());
                    ui.add_space(4.0);
                    for entry in &status.metadata.camera {
                        row(ui, entry.label, &entry.value, entry.label != "Lens");
                    }
                }

                if let Some(place) = status.metadata.location {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Place").strong());
                    ui.add_space(4.0);
                    row(ui, "Coordinates", &place.as_text(), true);
                }
            });
        });
}

/// One row of the panel: a label, a value, and a click that copies the value.
///
/// `measurable` picks the font: monospace for anything with digits in it, so
/// they do not jitter, which is the reference layout's rule.
fn row(ui: &mut egui::Ui, label: &str, value: &str, measurable: bool) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let text = if measurable {
                monospace(value.to_string())
            } else {
                egui::RichText::new(value)
            };
            // A label rather than a button: the panel is a reading surface,
            // and eighteen buttons in a column would read as a form.
            let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()).truncate());
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.on_hover_text("Click to copy").clicked() {
                ui.ctx().copy_text(value.to_string());
            }
        });
    });
}

/// A file size the way a person reads one.
fn file_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("MB", 1024 * 1024), ("kB", 1024), ("bytes", 1)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return if scale == 1 {
                format!("{bytes} {unit}")
            } else {
                format!("{:.1} {unit}", bytes as f64 / scale as f64)
            };
        }
    }
    format!("{bytes} bytes")
}

/// How tall the histogram's plot is, in logical points.
///
/// Tall enough for the shape of a curve to be legible, short enough that it
/// sits over a corner of the photograph rather than across it.
const HISTOGRAM_HEIGHT: f32 = 96.0;

/// How wide the histogram panel is.
///
/// One logical point per bucket plus its margins, so no column is dropped or
/// doubled by the rounding a narrower panel would force.
const HISTOGRAM_WIDTH: f32 = BUCKETS as f32 + 20.0;

/// How tall the whole panel comes out: the plot, the axis labels under it, and
/// the frame's own margins.
///
/// Used to place the panel from the bottom of the free area upwards, so it sits
/// on the status line rather than through it.
const HISTOGRAM_PANEL_HEIGHT: f32 = HISTOGRAM_HEIGHT + 34.0;

/// How much of the plot is left empty above the tallest column.
///
/// Without it the peak bucket runs into the ceiling and is drawn clipped, which
/// reads as a column that continues past the top — exactly the wrong thing for
/// a plot whose job is to show where the picture's values *stop*. Measured on a
/// flat-toned file, where one bucket holds nearly everything.
const HISTOGRAM_HEADROOM: f32 = 0.94;

// The two invariants the drawing depends on, held where the numbers are and
// checked as the crate builds rather than as a test runs: the peak column has
// to stay clear of the ceiling without wasting most of the plot, and the panel
// has to declare a height that covers the plot plus the labels under it — the
// figure it is placed by. Both were wrong in the build that reached a hands-on
// run, one clipping the tallest column and one hanging the panel through the
// status line.
const _: () = {
    assert!(HISTOGRAM_HEADROOM < 1.0, "the peak column is drawn right up to the ceiling");
    assert!(HISTOGRAM_HEADROOM > 0.8, "the plot wastes too much of its height");
    assert!(
        HISTOGRAM_PANEL_HEIGHT >= HISTOGRAM_HEIGHT + 30.0,
        "the panel does not cover the plot and the axis labels under it",
    );
};

/// What tones the picture is made of.
///
/// An overlay in the corner rather than a panel that takes a strip of window:
/// the picture keeps its framing while this is up, which is the same decision
/// the Info panel was built on — a histogram that reflows the photograph
/// changes the thing it is measuring.
///
/// It sits at the bottom left, clear of the Info panel on the right and of the
/// toolbar at the top, so the two can be read together.
///
/// Placed against the area the panels have left rather than against the window,
/// which is what a fixed offset from the bottom would do: the status line's
/// height is its text and its margins, so an offset guessed to clear it is a
/// guess that a different font size or a taller line silently invalidates.
/// Measured, the guess was already wrong — the panel hung past the bottom edge
/// and took the status line with it.
fn histogram_panel(ui: &mut egui::Ui, status: &Status) {
    // What is left of the window once the status line and any toolbar have
    // taken their strips. `available_rect_before_wrap` is what the panels
    // actually shrink; `max_rect` is the root allocation and still spans the
    // whole window, which is how the first attempt drew the panel through the
    // status line.
    let free = ui.available_rect_before_wrap();
    egui::Area::new("histogram".into())
        .fixed_pos(egui::pos2(free.left() + 12.0, free.bottom() - HISTOGRAM_PANEL_HEIGHT - 12.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(14, 16, 20, 235))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .corner_radius(6)
                .show(ui, |ui| {
                    ui.set_width(HISTOGRAM_WIDTH);
                    match &status.histogram {
                        Some(histogram) if !histogram.is_empty() => plot_histogram(ui, histogram),
                        // Counted, and there was nothing to count.
                        Some(_) => {
                            ui.label(egui::RichText::new("Tones").strong());
                            ui.label(egui::RichText::new("nothing but transparency").weak());
                        }
                        // The count is on its way. Said rather than shown as
                        // empty axes, which would read as a picture with no
                        // tones in it.
                        None => {
                            ui.label(egui::RichText::new("Tones").strong());
                            ui.label(egui::RichText::new("counting…").weak());
                        }
                    }
                });
        });
}

/// Draw the four curves.
///
/// The three channels are drawn in their own colours and **added** where they
/// overlap, so a grey picture reads as one white shape rather than as whichever
/// channel happened to be painted last. Drawn as opaque rectangles in order the
/// third channel simply covers the other two: measured on a grey gradient, the
/// whole plot came out blue, because blue is drawn last. Luminance goes over
/// the top in outline: it is the curve an exposure is judged by, and it has to
/// stay readable through whatever the channels are doing underneath.
fn plot_histogram(ui: &mut egui::Ui, histogram: &Histogram) {
    let (response, painter) = ui.allocate_painter(egui::vec2(BUCKETS as f32, HISTOGRAM_HEIGHT), egui::Sense::hover());
    let rect = response.rect;

    // One scale for all four curves: drawn against their own maxima, a flat
    // channel and a peaked one would look alike and a colour cast would
    // disappear. The headroom keeps the tallest column clear of the ceiling.
    let peak = histogram.peak().max(1) as f32;
    let width = rect.width() / BUCKETS as f32;
    let plot = |count: u32| (count as f32 / peak).min(1.0) * rect.height() * HISTOGRAM_HEADROOM;

    // The plot's own ground, so the curves are read against something rather
    // than against whatever part of the photograph is behind them.
    painter.rect_filled(rect, 2.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140));

    // One column per bucket, its colour mixed from whichever channels reach
    // that height. Walking the buckets rather than the channels is what makes
    // the addition possible: all three counts for a bucket are in hand at once.
    for bucket in 0..BUCKETS {
        let heights = [
            plot(histogram.channels[0][bucket]),
            plot(histogram.channels[1][bucket]),
            plot(histogram.channels[2][bucket]),
        ];
        let tallest = heights[0].max(heights[1]).max(heights[2]);
        if tallest <= 0.0 {
            continue;
        }

        // The column is drawn in bands from the bottom up, each band coloured
        // by the channels still present at that height. Sorting the three
        // heights gives the band boundaries directly.
        let mut steps = heights;
        steps.sort_by(f32::total_cmp);

        let x = rect.left() + bucket as f32 * width;
        let mut from = 0.0f32;
        for boundary in steps {
            if boundary <= from {
                continue;
            }
            // Which channels still reach into this band.
            let colour = band_colour([heights[0] >= boundary, heights[1] >= boundary, heights[2] >= boundary]);
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - boundary), egui::pos2(x + width, rect.bottom() - from)),
                0.0,
                colour,
            );
            from = boundary;
        }
    }

    // Luminance last, as a line over the channels rather than a filled shape,
    // so it is legible whatever they are doing underneath.
    let line: Vec<egui::Pos2> = histogram
        .luma
        .iter()
        .enumerate()
        .map(|(bucket, count)| egui::pos2(rect.left() + bucket as f32 * width + width / 2.0, rect.bottom() - plot(*count)))
        .collect();
    painter.add(egui::Shape::line(
        line,
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(240, 240, 245, 210)),
    ));

    ui.add_space(2.0);
    // What the axis means, because a histogram measured somewhere else answers
    // a different question — see the module for why it is the file's values.
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("shadows").weak().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new("highlights").weak().small());
        });
    });
}

/// The colour of one band of a histogram column: the channels present in it,
/// added.
///
/// This is what stops the plot from being a picture of whichever channel was
/// painted last. All three present is white — a neutral picture reads as one
/// grey shape — and any two make the secondary between them, so a cast shows
/// as the colour of the channels that are *missing* from a band.
fn band_colour(lit: [bool; 3]) -> egui::Color32 {
    /// How bright a channel is where it is present, and where it is not. The
    /// floor is not zero: a band with one channel in it still has to read as a
    /// column against the plot's dark ground.
    const ON: u8 = 235;
    const OFF: u8 = 30;

    egui::Color32::from_rgb(if lit[0] { ON } else { OFF }, if lit[1] { ON } else { OFF }, if lit[2] { ON } else { OFF })
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
    ("Z", "hold for 100% under the cursor"),
    ("L", "hold the framing across a step"),
    ("R", "turn a quarter clockwise (Shift for the other way)"),
    ("B", "what shows through transparency"),
    ("I", "what the file says about itself"),
    ("H", "what tones the picture is made of"),
    ("+ -", "zoom in / out"),
    ("F11", "full screen"),
    ("?", "this list"),
    ("Esc", "quit"),
];

/// What the toolbar is, said where a person looking for it would read it.
const TOOLBAR_HINT: &str = "Every one of these is on the toolbar too, which appears when the pointer reaches the top of the window.";

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
            locked: false,
            backdrop: None,
            metadata: Metadata::default(),
            path: None,
            file_size: None,
            histogram: None,
        }
    }

    /// The channels add rather than cover one another.
    ///
    /// Found by a hands-on run, not by a test: the plot was drawn as three
    /// passes of opaque rectangles, so blue — painted last — covered the other
    /// two and a grey gradient came out solid blue. The counting was right the
    /// whole time; the drawing was the lie.
    #[test]
    fn the_channels_add_where_they_overlap() {
        // All three present is neutral: a grey picture is one grey shape.
        let all = band_colour([true, true, true]);
        assert_eq!(all.r(), all.g(), "a neutral band came out tinted");
        assert_eq!(all.g(), all.b(), "a neutral band came out tinted");
        assert!(all.r() > 200, "a band with every channel in it is not bright");

        // Two channels make the secondary between them rather than the last
        // one drawn: red and green are yellow, not green.
        let yellow = band_colour([true, true, false]);
        assert!(yellow.r() > 200 && yellow.g() > 200, "red and green did not add");
        assert!(yellow.b() < 60, "a channel that is not in this band showed up in it");

        // And one channel alone is that channel, still legible against the
        // plot's dark ground.
        let blue = band_colour([false, false, true]);
        assert!(blue.b() > 200 && blue.r() < 60 && blue.g() < 60);
        assert!(blue.b() > 60, "a lone channel is too dark to read as a column");
    }

    /// Every combination is distinguishable from every other, or two different
    /// mixtures of channels would look the same and the plot would say less
    /// than it appears to.
    #[test]
    fn every_mixture_of_channels_looks_different() {
        let mut seen = Vec::new();
        for red in [false, true] {
            for green in [false, true] {
                for blue in [false, true] {
                    let colour = band_colour([red, green, blue]);
                    assert!(!seen.contains(&colour), "[{red}, {green}, {blue}] draws the same colour as another mixture",);
                    seen.push(colour);
                }
            }
        }
    }

    /// Where a named area actually landed, measured from what egui laid out.
    ///
    /// The panels decide their own heights from their text and margins, so the
    /// only honest way to ask whether two of them overlap is to lay a frame out
    /// and look at the rectangles that came back.
    fn laid_out_rect(interface: &mut Interface, status: &Status, id: egui::Id, height: f32) -> Option<egui::Rect> {
        // Twice: an `Area` reports the size it was on the previous pass, so the
        // first frame gives the position it took before its contents were
        // measured. The second is where it settles.
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, height))),
                ..Default::default()
            };
            let (mut output, _) = interface.layout(raw, status, Instant::now());
            output.textures_delta.clear();
        }
        interface.context().memory(|memory| memory.area_rect(id))
    }

    /// A histogram with something in it, so the panel lays out at the height it
    /// really draws at.
    fn counted_histogram() -> Histogram {
        let pixels: Vec<u8> = (0..64u32).flat_map(|index| [(index * 4) as u8, 128, 64, 255]).collect();
        Histogram::of(&crate::image_source::DecodedImage {
            width: 8,
            height: 8,
            pixels,
            depth: Depth::Eight,
        })
    }

    /// Where the area left to overlays ends, in a laid-out frame.
    ///
    /// Measured through the same call the panel places itself by, so the test
    /// cannot pass by agreeing with a number the drawing does not use.
    fn free_area_bottom(interface: &mut Interface, status: &Status, height: f32) -> f32 {
        let bottom = std::sync::Arc::new(std::sync::Mutex::new(height));
        let seen = std::sync::Arc::clone(&bottom);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, height))),
            ..Default::default()
        };
        let mut output = interface.context().clone().run_ui(raw, |ui| {
            status_line(ui, status);
            *seen.lock().expect("the probe is poisoned") = ui.available_rect_before_wrap().bottom();
        });
        output.textures_delta.clear();
        *bottom.lock().expect("the probe is poisoned")
    }

    /// The histogram sits above the status line rather than through it.
    ///
    /// Found by a hands-on run: the panel was anchored a guessed distance up
    /// from the bottom of the window, and the guess did not match what the
    /// status line actually occupies — so the axis labels were drawn straight
    /// over the file name. A guess is what this test exists to forbid; the
    /// heights belong to egui and only egui can be asked for them.
    #[test]
    fn the_histogram_does_not_cover_the_status_line() {
        for height in [500.0, 600.0, 900.0] {
            let mut interface = Interface::new();
            interface.toggle_histogram();
            // A counted histogram, not the "counting…" placeholder: the plot
            // is three times the height of that, and measuring the placeholder
            // is how a first version of this test passed while the panel drew
            // straight through the status line on screen.
            let mut status = status();
            status.histogram = Some(counted_histogram());

            let Some(plot) = laid_out_rect(&mut interface, &status, egui::Id::new("histogram"), height) else {
                panic!("the histogram laid out nothing at a window height of {height}");
            };
            assert!(
                plot.height() > HISTOGRAM_HEIGHT,
                "the panel measured {} tall, which is the placeholder rather than the plot",
                plot.height(),
            );

            // The status line takes a strip off the bottom, so anything the
            // panels have left ends above it. Asking egui for that is the
            // whole point: the strip's height is its text and its margins.
            let free_bottom = free_area_bottom(&mut interface, &status, height);
            assert!(
                free_bottom < height,
                "the status line took no strip at all, so this test would pass on any placement",
            );
            assert!(
                plot.bottom() <= free_bottom + 0.5,
                "at a window height of {height} the histogram reaches {} and the status line starts at {free_bottom}",
                plot.bottom(),
            );
            // And it is actually on screen, not pushed off the top by a
            // placement that only avoids the status line by leaving the window.
            assert!(plot.top() >= 0.0, "the histogram was pushed off the top of the window");
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
        // The whole strip, not just its left-hand end: the state buttons and
        // the full-screen control lay out from the right.
        for x in 4..896 {
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

    /// A press must reach the toolbar even though it changes no status.
    ///
    /// This is the defect that shipped in v0.17.0 and was found by hand, not
    /// by the suite: the interface was laid out only when its own digest
    /// changed, and a click on a button changes nothing in the digest — the
    /// file, the zoom, the mode are all what they were. So the frame carrying
    /// the press was never laid out and the button did nothing. The viewer
    /// asks egui whether it wants a frame now; this holds the property that
    /// made the answer necessary.
    #[test]
    fn a_press_changes_no_status_and_must_still_be_seen() {
        let mut interface = Interface::new();
        interface.follow_pointer(Some((400.0, 10.0)));
        let at = find_button(&mut interface, Action::Next);

        // Settle: lay out until the digest stops moving, which is the state
        // the viewer is in while someone reaches for a button.
        press(&mut interface, at, false);
        interface.changed(&status());
        assert!(!interface.changed(&status()), "the fixture never settled, so this proves nothing");

        // The press itself still has to be answered.
        assert_eq!(press(&mut interface, at, true), Some(Action::Next), "a press was lost on a settled interface");
    }

    /// Lay out one frame with the Info panel up, and report what it copied.
    fn info_frame(interface: &mut Interface, status: &Status, click: Option<egui::Pos2>) -> Vec<String> {
        let mut events = Vec::new();
        if let Some(at) = click {
            events.push(egui::Event::PointerMoved(at));
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
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
            events,
            ..Default::default()
        };
        let (mut output, _) = interface.layout(raw, status, Instant::now());
        output.textures_delta.clear();
        output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A status with the metadata a camera writes.
    fn photographed() -> Status {
        let mut status = status();
        status.metadata = Metadata {
            camera: vec![
                crate::metadata::Entry {
                    label: "Camera",
                    value: "NITID Probe One".into(),
                },
                crate::metadata::Entry {
                    label: "ISO",
                    value: "400".into(),
                },
            ],
            location: Some(crate::metadata::Location {
                latitude: -25.2637,
                longitude: -57.5759,
            }),
        };
        status.file_size = Some(3_500_000);
        status
    }

    /// Clicking a row copies its value. That is what makes the panel useful
    /// rather than only readable: a coordinate or a lens name is nearly always
    /// wanted somewhere else, and reading it off the screen to type it back in
    /// is the part that wastes it.
    #[test]
    fn a_row_of_the_panel_copies_what_it_shows() {
        let mut interface = Interface::new();
        interface.toggle_info();
        let status = photographed();

        // Settle, then walk the panel's own column looking for the row that
        // copies the coordinates.
        info_frame(&mut interface, &status, None);
        let wanted = crate::metadata::Location {
            latitude: -25.2637,
            longitude: -57.5759,
        }
        .as_text();

        let mut copied = Vec::new();
        for y in 20..680 {
            // The panel is 300 points wide at the right-hand edge of a 900
            // point window, so its values sit near the right.
            let at = egui::pos2(760.0, y as f32);
            info_frame(&mut interface, &status, None);
            copied.extend(info_frame(&mut interface, &status, Some(at)));
            if copied.contains(&wanted) {
                break;
            }
        }

        assert!(copied.contains(&wanted), "no row copied the coordinates; what was copied: {copied:?}",);
    }

    /// A panel that is not up copies nothing, whatever is clicked.
    #[test]
    fn a_hidden_panel_copies_nothing() {
        let mut interface = Interface::new();
        let status = photographed();
        info_frame(&mut interface, &status, None);

        let mut copied = Vec::new();
        for y in (20..680).step_by(7) {
            copied.extend(info_frame(&mut interface, &status, Some(egui::pos2(760.0, y as f32))));
        }
        assert!(copied.is_empty(), "a hidden panel copied {copied:?}");
    }

    /// The panel opening and closing is a change worth drawing.
    #[test]
    fn opening_the_panel_is_a_change() {
        let mut interface = Interface::new();
        interface.changed(&status());
        interface.toggle_info();
        assert!(interface.changed(&status()), "opening the panel went unnoticed");
        interface.toggle_info();
        assert!(interface.changed(&status()), "closing the panel went unnoticed");
    }

    /// A file size is read by a person, not by a machine.
    #[test]
    fn a_file_size_is_written_the_way_it_is_read() {
        assert_eq!(file_size(0), "0 bytes");
        assert_eq!(file_size(512), "512 bytes");
        assert_eq!(file_size(2048), "2.0 kB");
        assert_eq!(file_size(3_500_000), "3.3 MB");
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
        for wanted in [
            Action::Previous,
            Action::Next,
            Action::ZoomOut,
            Action::ZoomIn,
            Action::Fit,
            Action::Actual,
            Action::TurnLeft,
            Action::TurnRight,
            Action::Lock,
            Action::Backdrop,
            Action::Info,
            Action::FullScreen,
            Action::Keys,
        ] {
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
