//! Two pictures at once: the one to beat, pinned, and the one the arrow keys
//! walk.
//!
//! Choosing between the frames of a burst is not a question about one pair.
//! It is a walk through the series against whichever frame is best so far —
//! the shape Lightroom's Compare view has, and the one this follows: the
//! pinned picture stays put, the arrow keys move the other, and `Enter` makes
//! the other the new one to beat.
//!
//! Everything here is plain geometry and plain arithmetic, free of the window
//! and the GPU, for the reason `view.rs` is: the rules a person feels — which
//! way the window splits, which picture a blink is showing, how far apart two
//! exposures are — are then held by tests without a device.

use std::time::{Duration, Instant};

use crate::format::Format;
use crate::metadata::{self, Shot};

/// How long each picture stays up while the two are blinked.
///
/// Half a second: quick enough that the eye holds one picture while the other
/// arrives, which is what makes a moved eyelid or a shifted horizon jump out,
/// and slow enough to read the tag that says which one is up.
pub const BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// The gap between two panes, in logical points. The scene shows through it,
/// so two pictures zoomed past their panes still read as two.
pub const GUTTER: f32 = 4.0;

/// A pane of the window: left, top, width and height, in physical pixels.
pub type Rect = (u32, u32, u32, u32);

/// How the two pictures share the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Each in a pane of its own.
    Side,
    /// Both in the whole window, one at a time, alternating.
    Blink,
}

/// Which of the two a pane or a blink is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Pinned,
    Current,
}

/// Which way the window is divided between two panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// Side by side: pinned on the left.
    Across,
    /// One above the other: pinned on top.
    Down,
}

/// The split that shows the pinned picture larger.
///
/// A portrait frame side by side and a panorama one above the other: each is
/// the arrangement that wastes less of the window on the scene around the
/// picture. Decided by the pinned picture alone, which is what keeps the panes
/// still while the arrow keys walk the other one through frames of any shape.
/// A tie goes across, the way a person reads two things.
pub fn split_for(pinned: (u32, u32), window: (u32, u32), gutter: u32) -> Split {
    let (width, height) = (pinned.0.max(1) as f32, pinned.1.max(1) as f32);
    let (window_width, window_height) = (window.0 as f32, window.1 as f32);
    let half = |length: f32| ((length - gutter as f32) / 2.0).max(1.0);

    let across = (half(window_width) / width).min(window_height / height);
    let down = (window_width / width).min(half(window_height) / height);
    if down > across { Split::Down } else { Split::Across }
}

/// The two panes, pinned first, as `(left, top, width, height)` in physical
/// pixels.
///
/// The gutter goes between them and the odd pixel, when there is one, to the
/// second pane: both are then the same size to within a pixel, and a matched
/// framing is matched against the size it is drawn in.
pub fn panes(window: (u32, u32), split: Split, gutter: u32) -> [Rect; 2] {
    let (width, height) = window;
    match split {
        Split::Across => {
            let gutter = gutter.min(width);
            let first = (width - gutter) / 2;
            let second = width - gutter - first;
            [(0, 0, first, height), (first + gutter, 0, second, height)]
        }
        Split::Down => {
            let gutter = gutter.min(height);
            let first = (height - gutter) / 2;
            let second = height - gutter - first;
            [(0, 0, width, first), (0, first + gutter, width, second)]
        }
    }
}

/// Which pane a window position is in, and where in it.
///
/// A position in the gutter belongs to the nearer pane: a wheel notch or a
/// press that lands on the seam means one of the two pictures, never neither.
pub fn pane_at(panes: &[Rect; 2], split: Split, position: (f32, f32)) -> (Which, (f32, f32)) {
    let [first, second] = panes;
    // The middle of the gutter, worked in floats: a minimised window has
    // panes of no size at all, and unsigned arithmetic on them would wrap.
    let in_second = match split {
        Split::Across => position.0 >= ((first.0 + first.2) as f32 + second.0 as f32) / 2.0,
        Split::Down => position.1 >= ((first.1 + first.3) as f32 + second.1 as f32) / 2.0,
    };
    let (which, rect) = if in_second { (Which::Current, second) } else { (Which::Pinned, first) };
    (which, (position.0 - rect.0 as f32, position.1 - rect.1 as f32))
}

/// The clock of a blink.
#[derive(Clone, Copy, Debug)]
pub struct Blink {
    started: Instant,
}

impl Blink {
    pub fn new(now: Instant) -> Self {
        Self { started: now }
    }

    /// Which picture is up at `now`.
    ///
    /// The current one first: it is the one that was on screen when the blink
    /// was asked for, so the first thing that happens is the pinned one
    /// appearing over it — a change, which is what a blink is for.
    pub fn showing(&self, now: Instant) -> Which {
        let flips = now.saturating_duration_since(self.started).as_nanos() / BLINK_INTERVAL.as_nanos();
        if flips.is_multiple_of(2) { Which::Current } else { Which::Pinned }
    }

    /// When the picture up at `now` gives way to the other.
    pub fn next_flip(&self, now: Instant) -> Instant {
        let flips = now.saturating_duration_since(self.started).as_nanos() / BLINK_INTERVAL.as_nanos();
        // A blink running for longer than a `u32` of half-seconds — some
        // sixty years — starts counting again rather than overflowing.
        let flips = u32::try_from(flips + 1).unwrap_or(0);
        self.started + BLINK_INTERVAL * flips
    }
}

/// What one side of the comparison says about itself.
pub struct Side<'a> {
    pub size: Option<(u32, u32)>,
    pub format: Option<Format>,
    pub file_size: Option<u64>,
    pub shot: &'a Shot,
}

/// One line of the table: what differs, on each side, and by how much.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub pinned: String,
    pub current: String,
    /// How far the right-hand picture is from the left one, when that is a
    /// quantity: `+1 EV`, `+0.40 s`. For the exposure it is always said as
    /// light — how much more the current picture was given — whichever of
    /// shutter, aperture or ISO it came from, so the three read the same way.
    pub note: Option<String>,
}

/// The differences between two pictures, and what they share.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    pub rows: Vec<Row>,
    /// The fields both pictures state and agree on, by name.
    ///
    /// Named rather than dropped: "same ISO" is an answer too, and a table of
    /// differences with nothing else in it cannot tell a field that matched
    /// from one neither file carries.
    pub same: Vec<&'static str>,
}

/// What a field looks like on each side, and whether the two agree.
enum Field {
    /// Neither side says anything.
    Silent,
    Same,
    Differs {
        pinned: String,
        current: String,
        note: Option<String>,
    },
}

/// Compare two values of one field.
///
/// `equal` decides agreement — with a tolerance where the values are measured
/// rather than counted — `show` writes a value the way the Info panel does,
/// and `note` measures the distance between two present values.
fn field<T>(
    pinned: Option<&T>,
    current: Option<&T>,
    equal: impl Fn(&T, &T) -> bool,
    show: impl Fn(&T) -> String,
    note: impl Fn(&T, &T) -> Option<String>,
) -> Field {
    match (pinned, current) {
        (None, None) => Field::Silent,
        (Some(a), Some(b)) if equal(a, b) => Field::Same,
        (a, b) => Field::Differs {
            pinned: a.map_or_else(|| MISSING.to_string(), &show),
            current: b.map_or_else(|| MISSING.to_string(), &show),
            note: a.zip(b).and_then(|(a, b)| note(a, b)),
        },
    }
}

/// What a side shows for a field it does not state.
const MISSING: &str = "\u{2014}";

/// For a field whose values differ without a distance worth stating.
fn no_note<T>(_: &T, _: &T) -> Option<String> {
    None
}

/// A difference in light, in stops, the way compensation is written.
fn stops(value: f64) -> Option<String> {
    value.is_finite().then(|| metadata::compensation_text(value))
}

/// The distance between two pictures, field by field.
pub fn differences(pinned: &Side<'_>, current: &Side<'_>) -> Table {
    let (a, b) = (pinned.shot, current.shot);
    let close = |x: &f64, y: &f64| (x - y).abs() < 0.05;

    // The moment gets its own rules: the date is dropped when both share it,
    // because a burst differs in the fraction of a second and a full date on
    // each side would push that out of the column.
    let same_day = a.taken.as_ref().zip(b.taken.as_ref()).is_some_and(|(x, y)| x.day() == y.day());
    let moment = |taken: &metadata::Taken| if same_day { taken.time_text() } else { taken.text() };

    let fields = [
        ("Camera", field(a.camera.as_ref(), b.camera.as_ref(), |x, y| x == y, String::clone, no_note)),
        ("Lens", field(a.lens.as_ref(), b.lens.as_ref(), |x, y| x == y, String::clone, no_note)),
        (
            "Exposure",
            field(
                a.exposure.as_ref(),
                b.exposure.as_ref(),
                // Within a hundredth of a stop: 1/249.99 is the 1/250 on the dial.
                |x, y| (y / x).log2().abs() < 0.01,
                |x| metadata::exposure_text(*x),
                |x, y| stops((y / x).log2()),
            ),
        ),
        (
            "Aperture",
            field(
                a.aperture.as_ref(),
                b.aperture.as_ref(),
                close,
                |x| metadata::aperture_text(*x),
                // Light falls with the square of the f-number, so a stop is a
                // factor of the square root of two.
                |x, y| stops(2.0 * (x / y).log2()),
            ),
        ),
        (
            "ISO",
            field(
                a.iso.as_ref(),
                b.iso.as_ref(),
                |x, y| x == y,
                u32::to_string,
                |x, y| stops((f64::from(*y) / f64::from(*x)).log2()),
            ),
        ),
        (
            "Compensation",
            field(
                a.compensation.as_ref(),
                b.compensation.as_ref(),
                close,
                |x| metadata::compensation_text(*x),
                |x, y| stops(y - x),
            ),
        ),
        (
            "Focal length",
            field(
                a.focal_length.as_ref(),
                b.focal_length.as_ref(),
                close,
                |x| metadata::focal_text(*x, None),
                no_note,
            ),
        ),
        (
            "Taken",
            field(
                a.taken.as_ref(),
                b.taken.as_ref(),
                |x, y| x.seconds_until(y) == Some(0.0) || x.text() == y.text(),
                moment,
                |x, y| x.seconds_until(y).map(gap_text),
            ),
        ),
        (
            "Size",
            field(
                pinned.size.as_ref(),
                current.size.as_ref(),
                |x, y| x == y,
                |(width, height)| format!("{width} \u{d7} {height}"),
                no_note,
            ),
        ),
        (
            "Format",
            field(
                pinned.format.as_ref(),
                current.format.as_ref(),
                |x, y| x == y,
                |format| format.name().to_string(),
                no_note,
            ),
        ),
        (
            "File",
            field(
                pinned.file_size.as_ref(),
                current.file_size.as_ref(),
                |x, y| x == y,
                |bytes| crate::interface::file_size(*bytes),
                no_note,
            ),
        ),
    ];

    let mut table = Table::default();
    for (label, field) in fields {
        match field {
            Field::Silent => {}
            Field::Same => table.same.push(label),
            Field::Differs { pinned, current, note } => table.rows.push(Row { label, pinned, current, note }),
        }
    }
    table
}

/// How much later the current picture was taken, with its sign.
///
/// Tenths of a second at the scale of a burst, whole units beyond it: the
/// question below a second is "which frame of the burst", and above a minute
/// it is "which moment of the day", and neither is served by the other's
/// precision.
pub fn gap_text(seconds: f64) -> String {
    let sign = if seconds < 0.0 { "\u{2212}" } else { "+" };
    let magnitude = seconds.abs();
    let text = if magnitude < 10.0 {
        format!("{magnitude:.2} s")
    } else if magnitude < 60.0 {
        format!("{} s", magnitude.round())
    } else if magnitude < 3600.0 {
        let whole = magnitude.round() as u64;
        format!("{} min {} s", whole / 60, whole % 60)
    } else if magnitude < 86_400.0 {
        let whole = (magnitude / 60.0).round() as u64;
        format!("{} h {} min", whole / 60, whole % 60)
    } else {
        match (magnitude / 86_400.0).round() as u64 {
            1 => "1 day".to_string(),
            days => format!("{days} days"),
        }
    };
    format!("{sign}{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot() -> Shot {
        Shot {
            camera: Some("NITID Probe One".into()),
            lens: Some("NITID 35mm f/1.8".into()),
            exposure: Some(1.0 / 250.0),
            aperture: Some(2.8),
            iso: Some(400),
            compensation: Some(0.0),
            focal_length: Some(35.0),
            equivalent: Some(52),
            taken: Some(metadata::taken("2026:08:28 14:03:11", Some("20"), None)),
        }
    }

    fn side(shot: &Shot) -> Side<'_> {
        Side {
            size: Some((6000, 4000)),
            format: Some(Format::Jpeg),
            file_size: Some(8_000_000),
            shot,
        }
    }

    fn row<'a>(table: &'a Table, label: &str) -> Option<&'a Row> {
        table.rows.iter().find(|row| row.label == label)
    }

    /// Two frames of one burst: the same everything but the moment and the
    /// shutter. The table is those two rows, and the rest is named as shared.
    #[test]
    fn two_frames_of_a_burst_differ_in_what_changed_and_nothing_else() {
        let first = shot();
        let mut second = shot();
        second.exposure = Some(1.0 / 500.0);
        second.taken = Some(metadata::taken("2026:08:28 14:03:11", Some("60"), None));

        let table = differences(&side(&first), &side(&second));
        let labels: Vec<&str> = table.rows.iter().map(|row| row.label).collect();
        assert_eq!(labels, ["Exposure", "Taken"], "the table carries rows that did not change: {table:?}");
        assert_eq!(
            table.same,
            ["Camera", "Lens", "Aperture", "ISO", "Compensation", "Focal length", "Size", "Format", "File"]
        );

        let exposure = row(&table, "Exposure").expect("the shutter changed");
        assert_eq!((exposure.pinned.as_str(), exposure.current.as_str()), ("1/250 s", "1/500 s"));
        assert_eq!(exposure.note.as_deref(), Some("\u{2212}1 EV"), "half the shutter is a stop less light");

        let taken = row(&table, "Taken").expect("the moment changed");
        assert_eq!(
            (taken.pinned.as_str(), taken.current.as_str()),
            ("14:03:11.20", "14:03:11.60"),
            "the date was not dropped"
        );
        assert_eq!(taken.note.as_deref(), Some("+0.40 s"));
    }

    /// Shutter, aperture and ISO are three ways to the same light, so the
    /// note says each as light: more of it is plus, whichever control gave it.
    #[test]
    fn every_exposure_control_is_measured_as_light() {
        let first = shot();
        let mut brighter = shot();
        brighter.exposure = Some(1.0 / 125.0);
        brighter.aperture = Some(2.0);
        brighter.iso = Some(800);
        brighter.compensation = Some(0.7);

        let table = differences(&side(&first), &side(&brighter));
        let note = |label: &str| row(&table, label).and_then(|row| row.note.clone());
        assert_eq!(note("Exposure").as_deref(), Some("+1 EV"), "a longer shutter");
        assert_eq!(note("Aperture").as_deref(), Some("+1 EV"), "a wider aperture");
        assert_eq!(note("ISO").as_deref(), Some("+1 EV"), "a higher ISO");
        assert_eq!(note("Compensation").as_deref(), Some("+0.7 EV"));
    }

    /// A field one file states and the other does not is a difference, shown
    /// with a dash rather than dropped; a field neither states is not a row
    /// and not a match.
    #[test]
    fn a_field_only_one_side_states_is_a_difference() {
        let photograph = shot();
        let screenshot = Shot::default();
        let table = differences(&side(&photograph), &side(&screenshot));

        let camera = row(&table, "Camera").expect("the camera is on one side only");
        assert_eq!(camera.current, MISSING);
        assert_eq!(camera.note, None, "a difference was measured against nothing");
        assert!(!table.same.contains(&"Camera"));

        let neither = differences(&side(&screenshot), &side(&screenshot));
        assert!(neither.rows.is_empty(), "two silent files produced rows: {neither:?}");
        assert!(!neither.same.contains(&"ISO"), "a field neither file states was called the same");
    }

    /// Two moments on different days show their dates, or "14:03" beside
    /// "14:03" would read as the same moment.
    #[test]
    fn moments_on_different_days_keep_their_dates() {
        let first = shot();
        let mut later = shot();
        later.taken = Some(metadata::taken("2026:08:29 14:03:11", Some("20"), None));
        let table = differences(&side(&first), &side(&later));
        let taken = row(&table, "Taken").expect("a day apart");
        assert_eq!(taken.current, "2026-08-29 14:03:11.20");
        assert_eq!(taken.note.as_deref(), Some("+1 day"));
    }

    #[test]
    fn a_gap_is_written_at_the_scale_it_is_read_at() {
        assert_eq!(gap_text(0.4), "+0.40 s");
        assert_eq!(gap_text(-2.5), "\u{2212}2.50 s");
        assert_eq!(gap_text(42.4), "+42 s");
        assert_eq!(gap_text(125.0), "+2 min 5 s");
        assert_eq!(gap_text(5400.0), "+1 h 30 min");
    }

    /// A portrait frame sits side by side and a panorama one above the
    /// other: each is the split that draws the pinned picture larger.
    #[test]
    fn the_window_splits_the_way_that_shows_the_pinned_picture_larger() {
        let window = (1600, 1000);
        assert_eq!(split_for((2000, 3000), window, 4), Split::Across, "a portrait frame");
        assert_eq!(split_for((6000, 1000), window, 4), Split::Down, "a panorama");
        // 3:2 in a 16:10 window: across gives each pane 798 px of width,
        // which holds the frame at 0.266; down gives 498 px of height, 0.249.
        assert_eq!(split_for((3000, 2000), window, 4), Split::Across);
    }

    /// Two panes fill the window with the gutter between them and nothing
    /// lost or doubled at the seam.
    #[test]
    fn two_panes_tile_the_window_around_the_gutter() {
        for window in [(1600, 1000), (1601, 999), (3, 3)] {
            for split in [Split::Across, Split::Down] {
                let [first, second] = panes(window, split, 4);
                let gutter = 4.min(if split == Split::Across { window.0 } else { window.1 });
                match split {
                    Split::Across => {
                        assert_eq!(first.2 + gutter + second.2, window.0, "{window:?}: the widths do not add up");
                        assert_eq!(second.0, first.2 + gutter);
                        assert_eq!((first.3, second.3), (window.1, window.1));
                        assert!(second.2.abs_diff(first.2) <= 1, "{window:?}: the panes differ by more than a pixel");
                    }
                    Split::Down => {
                        assert_eq!(first.3 + gutter + second.3, window.1, "{window:?}: the heights do not add up");
                        assert_eq!(second.1, first.3 + gutter);
                        assert_eq!((first.2, second.2), (window.0, window.0));
                    }
                }
            }
        }
    }

    /// A position names the pane it is in and a place inside it, and the
    /// seam belongs to whichever pane is nearer.
    #[test]
    fn a_position_names_its_pane_and_the_place_in_it() {
        let across = panes((1000, 600), Split::Across, 4);
        assert_eq!(pane_at(&across, Split::Across, (100.0, 50.0)), (Which::Pinned, (100.0, 50.0)));
        let (which, local) = pane_at(&across, Split::Across, (704.0, 50.0));
        assert_eq!(which, Which::Current);
        assert_eq!(local, (704.0 - across[1].0 as f32, 50.0), "the place was not made local to the pane");
        assert_eq!(pane_at(&across, Split::Across, (497.0, 10.0)).0, Which::Pinned, "the near half of the seam");
        assert_eq!(pane_at(&across, Split::Across, (501.0, 10.0)).0, Which::Current, "the far half of the seam");

        let down = panes((1000, 600), Split::Down, 4);
        assert_eq!(pane_at(&down, Split::Down, (900.0, 100.0)).0, Which::Pinned);
        assert_eq!(pane_at(&down, Split::Down, (900.0, 500.0)).0, Which::Current);
    }

    /// The blink starts on the picture that was up, alternates on the
    /// interval, and says when it will next change.
    #[test]
    fn a_blink_alternates_on_its_interval() {
        let start = Instant::now();
        let blink = Blink::new(start);
        assert_eq!(blink.showing(start), Which::Current);
        assert_eq!(blink.showing(start + BLINK_INTERVAL / 2), Which::Current);
        assert_eq!(blink.showing(start + BLINK_INTERVAL), Which::Pinned);
        assert_eq!(blink.showing(start + BLINK_INTERVAL * 2 + BLINK_INTERVAL / 4), Which::Current);

        assert_eq!(blink.next_flip(start), start + BLINK_INTERVAL);
        assert_eq!(blink.next_flip(start + BLINK_INTERVAL * 3 / 2), start + BLINK_INTERVAL * 2);
    }
}
