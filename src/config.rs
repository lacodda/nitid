//! What the viewer remembers between runs: where its window was, and every
//! choice the settings dialog offers.
//!
//! It lives in `%APPDATA%\lacodda\nitid`, written as a handful of
//! `key = value` lines: a viewer that needs a TOML parser to remember where
//! its window was is carrying a dependency for nothing, and the file stays
//! readable to anyone who opens it.
//!
//! Every failure here is silent. A viewer that refuses to open because its
//! settings file is unreadable has its priorities backwards; the defaults are
//! always a usable answer.
//!
//! A setting the file does not mention takes its default, and one this
//! version does not know is carried through untouched rather than dropped:
//! opening an older build must not silently empty a newer build's settings.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Where the window was when it was last closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Placement {
    /// Top-left corner in physical pixels, as the desktop measures it.
    pub position: Option<(i32, i32)>,
    /// Inner size in physical pixels.
    pub size: Option<(u32, u32)>,
    pub maximised: bool,
}

/// What the wheel does when it is turned with no modifier held.
///
/// The other gesture is always available on Ctrl+wheel, so this chooses which
/// of the two is the bare one rather than which of them exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Wheel {
    /// Zoom around the cursor.
    #[default]
    Zoom,
    /// Step to the next or previous image in the folder.
    Step,
}

impl Wheel {
    /// The gesture Ctrl+wheel performs: whichever one the bare wheel does not.
    pub fn modified(self) -> Self {
        match self {
            Self::Zoom => Self::Step,
            Self::Step => Self::Zoom,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "zoom" => Some(Self::Zoom),
            "step" => Some(Self::Step),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Zoom => "zoom",
            Self::Step => "step",
        }
    }
}

/// When a strip of chrome is on screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Chrome {
    /// Visible whenever the pointer reaches for it.
    #[default]
    Hover,
    /// Always on screen.
    Always,
    /// Never shown.
    Never,
}

impl Chrome {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "hover" => Some(Self::Hover),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Hover => "hover",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// How an image is framed when it arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Opening {
    /// Fit inside the window, never enlarging beyond 100%.
    #[default]
    Fit,
    /// One image pixel per logical screen pixel.
    Actual,
}

impl Opening {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "fit" => Some(Self::Fit),
            "actual" => Some(Self::Actual),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Fit => "fit",
            Self::Actual => "actual",
        }
    }
}

/// The order a folder's images are stepped through in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Order {
    /// By file name, case-insensitively.
    #[default]
    Name,
    /// Most recently modified first.
    Modified,
    /// Largest file first.
    Size,
}

impl Order {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "name" => Some(Self::Name),
            "modified" => Some(Self::Modified),
            "size" => Some(Self::Size),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Modified => "modified",
            Self::Size => "size",
        }
    }
}

/// The units the eyedropper reports a colour in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Units {
    /// Eight-bit channels, 0..255.
    #[default]
    Bytes,
    /// Percentages of full scale.
    Percent,
    /// `#RRGGBB`.
    Hex,
}

impl Units {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "bytes" => Some(Self::Bytes),
            "percent" => Some(Self::Percent),
            "hex" => Some(Self::Hex),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Percent => "percent",
            Self::Hex => "hex",
        }
    }
}

/// What a click in the eyedropper puts on the clipboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Copies {
    /// `#RRGGBB`, whatever the panel is showing.
    #[default]
    Hex,
    /// The three channels in the units the panel shows.
    Channels,
}

impl Copies {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "hex" => Some(Self::Hex),
            "channels" => Some(Self::Channels),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Hex => "hex",
            Self::Channels => "channels",
        }
    }
}

/// How the wheel and the mouse behave.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Gestures {
    /// What a bare wheel does; Ctrl+wheel does the other.
    pub wheel: Wheel,
    /// Whether the wheel's direction is reversed.
    pub invert_wheel: bool,
    /// What one notch multiplies the scale by. Above 1.0.
    pub zoom_step: f32,
    /// Whether the middle button toggles between fit and 100%.
    pub middle_toggles: bool,
}

impl Default for Gestures {
    fn default() -> Self {
        Self {
            wheel: Wheel::default(),
            invert_wheel: false,
            zoom_step: DEFAULT_ZOOM_STEP,
            middle_toggles: true,
        }
    }
}

/// The default zoom per wheel notch, kept here so the dialog and the view
/// agree on what "the usual" means.
pub const DEFAULT_ZOOM_STEP: f32 = 1.1;

/// How far the zoom step may be pushed either way.
///
/// Below the floor a notch does nothing perceptible; above the ceiling one
/// notch crosses the whole useful zoom range and the wheel stops being a
/// control.
pub const MIN_ZOOM_STEP: f32 = 1.01;
pub const MAX_ZOOM_STEP: f32 = 2.0;

/// What is on screen besides the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Appearance {
    pub toolbar: Chrome,
    pub status_line: Chrome,
}

/// How an image is framed on arrival, and what the folder does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Behaviour {
    pub opening: Opening,
    /// Whether the framing is held across a step by default. The `L` key
    /// still toggles it for the session.
    pub hold_zoom: bool,
    /// Whether stepping past the last image comes back to the first.
    pub wrap: bool,
    pub order: Order,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            opening: Opening::default(),
            hold_zoom: false,
            wrap: true,
            order: Order::default(),
        }
    }
}

/// The colour tools' own settings.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Tools {
    /// At or above this fraction of full scale a highlight is called blown.
    pub clip_high: f32,
    /// At or below this fraction a shadow is called blocked.
    pub clip_low: f32,
    pub units: Units,
    pub copies: Copies,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            clip_high: DEFAULT_CLIP_HIGH,
            clip_low: DEFAULT_CLIP_LOW,
            units: Units::default(),
            copies: Copies::default(),
        }
    }
}

/// The zebra's default thresholds, matching what the shader judged on before
/// they were adjustable: a whisker below the ends rather than exactly at them.
pub const DEFAULT_CLIP_HIGH: f32 = 0.996;
pub const DEFAULT_CLIP_LOW: f32 = 0.004;

/// The settings as they stand.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Config {
    pub placement: Placement,
    pub gestures: Gestures,
    pub appearance: Appearance,
    pub behaviour: Behaviour,
    pub tools: Tools,
    /// Keys the file carried that this version does not know.
    ///
    /// Kept so that saving does not throw away a newer version's settings:
    /// the file is one file, and both builds write all of it.
    unknown: BTreeMap<String, String>,
}

impl Config {
    /// Read the settings, falling back to defaults for anything missing.
    pub fn load() -> Self {
        let Some(text) = path().and_then(|path| fs::read_to_string(path).ok()) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    /// Write the settings, ignoring a failure to do so.
    ///
    /// Losing a window position is a small enough matter that it must never
    /// interrupt closing the viewer.
    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(path, self.render());
    }

    fn parse(text: &str) -> Self {
        let mut config = Self::default();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());

            match key {
                "window_position" => config.placement.position = parse_pair(value),
                "window_size" => {
                    config.placement.size = parse_pair(value)
                        // A stored size of zero would make a window nobody can
                        // see; treat it as absent.
                        .filter(|(width, height)| *width > 0 && *height > 0)
                        .map(|(width, height)| (width as u32, height as u32));
                }
                "window_maximised" => config.placement.maximised = value == "true",

                "wheel" => config.gestures.wheel = Wheel::parse(value).unwrap_or_default(),
                "invert_wheel" => config.gestures.invert_wheel = value == "true",
                "zoom_step" => {
                    if let Ok(step) = value.parse::<f32>()
                        && step.is_finite()
                    {
                        config.gestures.zoom_step = step.clamp(MIN_ZOOM_STEP, MAX_ZOOM_STEP);
                    }
                }
                "middle_toggles" => config.gestures.middle_toggles = value != "false",

                "toolbar" => config.appearance.toolbar = Chrome::parse(value).unwrap_or_default(),
                "status_line" => config.appearance.status_line = Chrome::parse(value).unwrap_or_default(),

                "opening" => config.behaviour.opening = Opening::parse(value).unwrap_or_default(),
                "hold_zoom" => config.behaviour.hold_zoom = value == "true",
                "wrap" => config.behaviour.wrap = value != "false",
                "order" => config.behaviour.order = Order::parse(value).unwrap_or_default(),

                "clip_high" => config.tools.clip_high = parse_fraction(value).unwrap_or(DEFAULT_CLIP_HIGH),
                "clip_low" => config.tools.clip_low = parse_fraction(value).unwrap_or(DEFAULT_CLIP_LOW),
                "units" => config.tools.units = Units::parse(value).unwrap_or_default(),
                "copies" => config.tools.copies = Copies::parse(value).unwrap_or_default(),

                // Not a key this version knows. It belongs to a build that
                // wrote the file before or after this one; either way it is
                // not this version's to discard.
                _ => {
                    config.unknown.insert(key.to_string(), value.to_string());
                }
            }
        }

        config
    }

    fn render(&self) -> String {
        let mut out = String::from("# nitid settings\n");

        if let Some((x, y)) = self.placement.position {
            out.push_str(&format!("window_position = {x}, {y}\n"));
        }
        if let Some((width, height)) = self.placement.size {
            out.push_str(&format!("window_size = {width}, {height}\n"));
        }
        out.push_str(&format!("window_maximised = {}\n", self.placement.maximised));

        out.push_str(&format!("wheel = {}\n", self.gestures.wheel.render()));
        out.push_str(&format!("invert_wheel = {}\n", self.gestures.invert_wheel));
        out.push_str(&format!("zoom_step = {}\n", self.gestures.zoom_step));
        out.push_str(&format!("middle_toggles = {}\n", self.gestures.middle_toggles));

        out.push_str(&format!("toolbar = {}\n", self.appearance.toolbar.render()));
        out.push_str(&format!("status_line = {}\n", self.appearance.status_line.render()));

        out.push_str(&format!("opening = {}\n", self.behaviour.opening.render()));
        out.push_str(&format!("hold_zoom = {}\n", self.behaviour.hold_zoom));
        out.push_str(&format!("wrap = {}\n", self.behaviour.wrap));
        out.push_str(&format!("order = {}\n", self.behaviour.order.render()));

        out.push_str(&format!("clip_high = {}\n", self.tools.clip_high));
        out.push_str(&format!("clip_low = {}\n", self.tools.clip_low));
        out.push_str(&format!("units = {}\n", self.tools.units.render()));
        out.push_str(&format!("copies = {}\n", self.tools.copies.render()));

        for (key, value) in &self.unknown {
            out.push_str(&format!("{key} = {value}\n"));
        }

        out
    }
}

fn parse_pair(value: &str) -> Option<(i32, i32)> {
    let (first, second) = value.split_once(',')?;
    Some((first.trim().parse().ok()?, second.trim().parse().ok()?))
}

/// A threshold in 0..1, rejecting anything outside it.
///
/// A zebra told to mark everything at or above zero would paint the whole
/// picture, which reads as a broken viewer rather than as a setting.
fn parse_fraction(value: &str) -> Option<f32> {
    let parsed = value.parse::<f32>().ok()?;
    (parsed.is_finite() && (0.0..=1.0).contains(&parsed)).then_some(parsed)
}

/// `%APPDATA%\lacodda\nitid\settings.conf`.
fn path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("lacodda").join("nitid").join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_there_is_nothing_stored() {
        let config = Config::parse("");
        assert_eq!(config.placement.position, None);
        assert_eq!(config.placement.size, None);
        assert!(!config.placement.maximised);
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_placement_survives_a_round_trip() {
        let config = Config {
            placement: Placement {
                position: Some((120, -40)),
                size: Some((1600, 900)),
                maximised: false,
            },
            ..Config::default()
        };
        assert_eq!(Config::parse(&config.render()), config);
    }

    /// A window on a monitor left of the primary one has a negative x; that is
    /// a real position, not corrupt data.
    #[test]
    fn negative_coordinates_are_kept() {
        let config = Config::parse("window_position = -1920, 300");
        assert_eq!(config.placement.position, Some((-1920, 300)));
    }

    #[test]
    fn a_maximised_window_is_remembered_as_such() {
        let config = Config::parse("window_maximised = true");
        assert!(config.placement.maximised);
    }

    #[test]
    fn nonsense_is_ignored_rather_than_fatal() {
        let config = Config::parse(
            "# a comment\n\
             window_position = not a pair\n\
             window_size = 0, 0\n\
             malformed line without a separator\n",
        );
        assert_eq!(config.placement.position, None);
        // A zero size would open a window with nothing in it.
        assert_eq!(config.placement.size, None);
    }

    #[test]
    fn every_setting_survives_a_round_trip() {
        let config = Config {
            placement: Placement::default(),
            gestures: Gestures {
                wheel: Wheel::Step,
                invert_wheel: true,
                zoom_step: 1.35,
                middle_toggles: false,
            },
            appearance: Appearance {
                toolbar: Chrome::Always,
                status_line: Chrome::Never,
            },
            behaviour: Behaviour {
                opening: Opening::Actual,
                hold_zoom: true,
                wrap: false,
                order: Order::Modified,
            },
            tools: Tools {
                clip_high: 0.98,
                clip_low: 0.02,
                units: Units::Percent,
                copies: Copies::Channels,
            },
            unknown: BTreeMap::new(),
        };
        assert_eq!(Config::parse(&config.render()), config);
    }

    /// The defaults have to survive the file too: a viewer that wrote its
    /// defaults out and read something else back would drift a little on
    /// every run.
    #[test]
    fn the_defaults_survive_a_round_trip() {
        let config = Config::default();
        assert_eq!(Config::parse(&config.render()), config);
    }

    #[test]
    fn a_word_the_setting_does_not_know_falls_back_to_its_default() {
        let config = Config::parse("wheel = sideways\norder = colour\nunits = furlongs\n");
        assert_eq!(config.gestures.wheel, Wheel::default());
        assert_eq!(config.behaviour.order, Order::default());
        assert_eq!(config.tools.units, Units::default());
    }

    /// A wheel that multiplied the scale by 40 a notch, or by 1.0000001, is a
    /// wheel nobody can aim; the file is not allowed to ask for one.
    #[test]
    fn an_impossible_zoom_step_is_pulled_back_into_range() {
        assert_eq!(Config::parse("zoom_step = 40").gestures.zoom_step, MAX_ZOOM_STEP);
        assert_eq!(Config::parse("zoom_step = 1.0").gestures.zoom_step, MIN_ZOOM_STEP);
        assert_eq!(Config::parse("zoom_step = nonsense").gestures.zoom_step, DEFAULT_ZOOM_STEP);
        // An infinity parses as a float and would survive a range check
        // written as a comparison; it must not reach the view.
        assert_eq!(Config::parse("zoom_step = inf").gestures.zoom_step, DEFAULT_ZOOM_STEP);
    }

    /// A threshold outside 0..1 cannot mean anything to a zebra judging
    /// stored values, and one at the very ends would mark everything.
    #[test]
    fn a_threshold_outside_the_range_falls_back_to_its_default() {
        assert_eq!(Config::parse("clip_high = 1.5").tools.clip_high, DEFAULT_CLIP_HIGH);
        assert_eq!(Config::parse("clip_low = -0.2").tools.clip_low, DEFAULT_CLIP_LOW);
        assert_eq!(Config::parse("clip_high = nonsense").tools.clip_high, DEFAULT_CLIP_HIGH);
    }

    /// The booleans that default to on have to be turned off by the word
    /// `false` and by nothing else: parsing them as "anything but true" would
    /// make a typo silently disable them.
    #[test]
    fn the_settings_that_default_to_on_stay_on_unless_denied() {
        assert!(Config::parse("wrap = yes").behaviour.wrap);
        assert!(Config::parse("middle_toggles = yes").gestures.middle_toggles);
        assert!(!Config::parse("wrap = false").behaviour.wrap);
        assert!(!Config::parse("middle_toggles = false").gestures.middle_toggles);
    }

    /// A file written by a later version keeps its settings through a run of
    /// this one. Without this, opening an old build once would quietly empty
    /// everything the new build had stored.
    #[test]
    fn a_key_this_version_does_not_know_survives_being_rewritten() {
        let config = Config::parse("window_size = 800, 600\nfuture_setting = 1\n");
        assert_eq!(config.placement.size, Some((800, 600)));

        let rewritten = config.render();
        assert!(rewritten.contains("future_setting = 1"), "the unknown key was dropped: {rewritten}");
        assert_eq!(Config::parse(&rewritten), config);
    }

    /// Ctrl+wheel is always the gesture the bare wheel is not, so that both
    /// are reachable whichever way round the setting is.
    #[test]
    fn the_modifier_always_offers_the_other_gesture() {
        assert_eq!(Wheel::Zoom.modified(), Wheel::Step);
        assert_eq!(Wheel::Step.modified(), Wheel::Zoom);
    }
}
