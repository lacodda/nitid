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

use crate::gpu::Backdrop;

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

/// When the minimap is on screen.
///
/// Not [`Chrome`], though both answer "when is this shown": chrome appears
/// when the pointer reaches for it, and the minimap's middle answer is about
/// the picture rather than the pointer. A minimap is only ever useful when
/// part of the image is off screen, so the default shows it exactly then and
/// keeps a fitted photograph clear of furniture.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Minimap {
    /// Only while part of the picture is off screen.
    #[default]
    Zoomed,
    /// Whenever an image is open, even wholly visible.
    Always,
    /// Never.
    Never,
}

impl Minimap {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "zoomed" => Some(Self::Zoomed),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    fn render(self) -> &'static str {
        match self {
            Self::Zoomed => "zoomed",
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
    /// When the minimap shows where in the picture the window is.
    pub minimap: Minimap,
    /// What shows through a transparent pixel when an image is opened.
    ///
    /// The `B` key still walks the four for the session; this is where it
    /// starts. A cut-out judged against one backdrop is a cut-out judged
    /// against one background, so which one you start from is a working
    /// preference rather than a detail.
    pub backdrop: Backdrop,
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
    /// Whether the eyedropper shows the pixels around the one it reads,
    /// magnified, or only the one.
    ///
    /// On by default: the neighbourhood is what makes a reading trustworthy at
    /// a zoom where a pixel is smaller than the pointer. The single swatch
    /// stays as a choice for someone who wants the panel small.
    pub magnifier: bool,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            clip_high: DEFAULT_CLIP_HIGH,
            clip_low: DEFAULT_CLIP_LOW,
            units: Units::default(),
            copies: Copies::default(),
            magnifier: true,
        }
    }
}

/// The zebra's default thresholds, matching what the shader judged on before
/// they were adjustable: a whisker below the ends rather than exactly at them.
pub const DEFAULT_CLIP_HIGH: f32 = 0.996;
pub const DEFAULT_CLIP_LOW: f32 = 0.004;

/// How many folders a picture can be sorted into by key.
///
/// Nine, because the keys are the digits: `Ctrl+1` through `Ctrl+9` move a
/// file, and holding Shift copies it instead. `0` is not one of them — it
/// fits the picture to the window, and `1` shows it at 100%, both since
/// v0.1.0. The digits carry a modifier for exactly that reason (owner,
/// 2026-09-09): a sorting key added now does not get to take a viewing
/// gesture that has been there since the beginning.
pub const SORTING_FOLDERS: usize = 9;

/// How many programs the number keys can hold, one per digit.
///
/// The same nine as the sorting folders, and on the same reasoning: the digit
/// row is what a hand finds without looking. They do not collide because the
/// modifier differs — `Ctrl` sorts, `Alt` hands the picture to a program.
pub const PROGRAMS: usize = 9;

/// The programs the number keys start, and the editor `E` uses.
///
/// Empty is the normal state of all of them. An unset key says so when it is
/// pressed rather than doing something surprising, and `E` with no program set
/// is not unset at all — it asks Windows for whatever edits this kind of file,
/// which is what makes the key work on a viewer nobody has configured.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Programs {
    /// Index 0 is `Alt+1`. Empty means "no program set for that key".
    pub commands: [PathBuf; PROGRAMS],
    /// What `E` starts. Empty means "ask Windows", which is the default and
    /// the reason the key is useful before anyone opens the settings.
    pub editor: PathBuf,
}

impl Programs {
    /// The program for a digit key, or `None` when nothing is set for it.
    ///
    /// `digit` is what the person pressed: 1 through 9.
    pub fn command(&self, digit: usize) -> Option<&std::path::Path> {
        let program = self.commands.get(digit.checked_sub(1)?)?;
        (!program.as_os_str().is_empty()).then_some(program.as_path())
    }

    /// The editor `E` should start, or `None` to let Windows choose.
    pub fn editor(&self) -> Option<&std::path::Path> {
        (!self.editor.as_os_str().is_empty()).then_some(self.editor.as_path())
    }

    /// Whether any program at all is set, which is what decides whether the
    /// key sheet mentions the number keys.
    pub fn any(&self) -> bool {
        self.commands.iter().any(|program| !program.as_os_str().is_empty())
    }
}

/// Where the digit keys put a picture.
///
/// Empty is the normal state of most of them: someone who sorts into two
/// folders sets two, and the other seven say so when pressed rather than
/// doing something surprising.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Sorting {
    /// Index 0 is the key `1`. Empty means "no folder set for that key".
    pub folders: [PathBuf; SORTING_FOLDERS],
}

impl Sorting {
    /// The folder for a digit key, or `None` when nothing is set for it.
    ///
    /// `digit` is what the person pressed: 1 through 9.
    pub fn folder(&self, digit: usize) -> Option<&std::path::Path> {
        let folder = self.folders.get(digit.checked_sub(1)?)?;
        (!folder.as_os_str().is_empty()).then_some(folder.as_path())
    }

    /// Whether any folder at all is set, which is what decides whether the
    /// key sheet mentions sorting.
    pub fn any(&self) -> bool {
        self.folders.iter().any(|folder| !folder.as_os_str().is_empty())
    }
}

/// The settings as they stand.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Config {
    pub placement: Placement,
    pub gestures: Gestures,
    pub appearance: Appearance,
    pub behaviour: Behaviour,
    pub tools: Tools,
    pub sorting: Sorting,
    pub programs: Programs,
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

                "backdrop" => config.appearance.backdrop = Backdrop::from_keyword(value).unwrap_or_default(),
                "toolbar" => config.appearance.toolbar = Chrome::parse(value).unwrap_or_default(),
                "status_line" => config.appearance.status_line = Chrome::parse(value).unwrap_or_default(),
                "minimap" => config.appearance.minimap = Minimap::parse(value).unwrap_or_default(),

                "opening" => config.behaviour.opening = Opening::parse(value).unwrap_or_default(),
                "hold_zoom" => config.behaviour.hold_zoom = value == "true",
                "wrap" => config.behaviour.wrap = value != "false",
                "order" => config.behaviour.order = Order::parse(value).unwrap_or_default(),

                "clip_high" => config.tools.clip_high = parse_fraction(value).unwrap_or(DEFAULT_CLIP_HIGH),
                "clip_low" => config.tools.clip_low = parse_fraction(value).unwrap_or(DEFAULT_CLIP_LOW),
                "units" => config.tools.units = Units::parse(value).unwrap_or_default(),
                "copies" => config.tools.copies = Copies::parse(value).unwrap_or_default(),
                "magnifier" => config.tools.magnifier = value != "false",

                // The sorting folders, one key each. The value is taken as it
                // stands: a path holds spaces, and trimming beyond the ends
                // would quietly change where a picture goes.
                _ if key.starts_with("folder_") => {
                    if let Some(digit) = key.strip_prefix("folder_").and_then(|digit| digit.parse::<usize>().ok())
                        && (1..=SORTING_FOLDERS).contains(&digit)
                    {
                        config.sorting.folders[digit - 1] = PathBuf::from(value);
                    }
                }
                // The same as the folders above, and untrimmed for the same
                // reason: a program lives at a path with spaces in it.
                "editor" => config.programs.editor = PathBuf::from(value),
                _ if key.starts_with("program_") => {
                    if let Some(digit) = key.strip_prefix("program_").and_then(|digit| digit.parse::<usize>().ok())
                        && (1..=PROGRAMS).contains(&digit)
                    {
                        config.programs.commands[digit - 1] = PathBuf::from(value);
                    }
                }

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

        out.push_str(&format!("backdrop = {}\n", self.appearance.backdrop.keyword()));
        out.push_str(&format!("toolbar = {}\n", self.appearance.toolbar.render()));
        out.push_str(&format!("status_line = {}\n", self.appearance.status_line.render()));
        out.push_str(&format!("minimap = {}\n", self.appearance.minimap.render()));

        out.push_str(&format!("opening = {}\n", self.behaviour.opening.render()));
        out.push_str(&format!("hold_zoom = {}\n", self.behaviour.hold_zoom));
        out.push_str(&format!("wrap = {}\n", self.behaviour.wrap));
        out.push_str(&format!("order = {}\n", self.behaviour.order.render()));

        out.push_str(&format!("clip_high = {}\n", self.tools.clip_high));
        out.push_str(&format!("clip_low = {}\n", self.tools.clip_low));
        out.push_str(&format!("units = {}\n", self.tools.units.render()));
        out.push_str(&format!("copies = {}\n", self.tools.copies.render()));
        out.push_str(&format!("magnifier = {}\n", self.tools.magnifier));

        // Only the ones that are set: a file listing nine empty keys says
        // nothing and invites someone to fill them in by hand wrongly.
        for (index, folder) in self.sorting.folders.iter().enumerate() {
            if !folder.as_os_str().is_empty() {
                out.push_str(&format!("folder_{} = {}\n", index + 1, folder.display()));
            }
        }
        if !self.programs.editor.as_os_str().is_empty() {
            out.push_str(&format!("editor = {}\n", self.programs.editor.display()));
        }
        for (index, program) in self.programs.commands.iter().enumerate() {
            if !program.as_os_str().is_empty() {
                out.push_str(&format!("program_{} = {}\n", index + 1, program.display()));
            }
        }

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
    use std::path::Path;

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

    /// A path is the one setting whose value is arbitrary text: it holds
    /// spaces, and mangling it would send a picture somewhere else.
    #[test]
    fn a_sorting_folder_keeps_the_path_it_was_given() {
        let config = Config::parse("folder_1 = C:\\Users\\someone\\My Pictures\\keepers\n");
        assert_eq!(config.sorting.folder(1), Some(Path::new("C:\\Users\\someone\\My Pictures\\keepers")));
    }

    #[test]
    fn a_folder_that_is_not_set_is_none_rather_than_an_empty_path() {
        let config = Config::parse("folder_1 = C:\\keep\n");
        assert!(config.sorting.folder(2).is_none(), "an unset key named a folder");
        assert!(config.sorting.any(), "a set folder was not noticed");
        assert!(!Config::default().sorting.any(), "a fresh config claims to have folders");
    }

    /// The digits are 1..=9, and anything else in the file is not a folder
    /// this version knows — `folder_0` would mean the fit-to-window key.
    #[test]
    fn only_the_nine_digits_name_a_folder() {
        let config = Config::parse("folder_0 = C:\\no\nfolder_10 = C:\\no\nfolder_x = C:\\no\n");
        assert!(!config.sorting.any(), "a key outside 1..=9 set a folder");
        assert!(config.sorting.folder(0).is_none(), "digit 0 named a folder");
        assert!(config.sorting.folder(10).is_none());
    }

    /// Only the folders that are set are written, and they come back the same.
    #[test]
    fn the_folders_survive_a_round_trip() {
        let mut config = Config::default();
        config.sorting.folders[0] = PathBuf::from("C:\\keep");
        config.sorting.folders[8] = PathBuf::from("D:\\some folder\\reject");

        let rendered = config.render();
        assert!(rendered.contains("folder_1 = C:\\keep"), "{rendered}");
        assert!(rendered.contains("folder_9 = D:\\some folder\\reject"), "{rendered}");
        // The seven that are not set say nothing at all.
        assert!(!rendered.contains("folder_2"), "an unset folder was written: {rendered}");

        assert_eq!(Config::parse(&rendered), config);
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
                minimap: Minimap::Always,
                backdrop: Backdrop::Checker,
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
                magnifier: false,
            },
            // A folder set and a folder left alone, so the round trip covers
            // both: writing every key would be a different bug from writing
            // none of them.
            sorting: Sorting {
                folders: [
                    PathBuf::from("C:\\keep"),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::from("D:\\reject"),
                ],
            },
            // A program set at each end and the middle left alone, on the same
            // reasoning as the folders above. The editor carries a path with a
            // space in it, which is where a program normally lives.
            programs: Programs {
                commands: [
                    PathBuf::from("C:/Program Files/Some Editor/editor.exe"),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::new(),
                    PathBuf::from("D:/tools/stamp.exe"),
                ],
                editor: PathBuf::from("C:/Program Files/Paint Something/paint.exe"),
            },
            unknown: BTreeMap::new(),
        };
        assert_eq!(Config::parse(&config.render()), config);
    }

    /// A program keeps the path it was given, spaces and all.
    ///
    /// Programs live under `Program Files` more often than not, so a value
    /// trimmed past its ends would break the common case rather than an
    /// unusual one.
    #[test]
    fn a_program_keeps_the_path_it_was_given() {
        let config = Config::parse("program_1 = C:/Program Files/Some Editor/editor.exe");
        assert_eq!(
            config.programs.command(1),
            Some(std::path::Path::new("C:/Program Files/Some Editor/editor.exe")),
        );
    }

    /// A key with nothing on it is `None`, not an empty path.
    ///
    /// The caller decides what to say about an unset key, and it can only do
    /// that if the setting admits to being unset.
    #[test]
    fn a_program_that_is_not_set_is_none_rather_than_an_empty_path() {
        let config = Config::parse("program_1 = C:/tools/one.exe");
        assert_eq!(config.programs.command(2), None);
        assert_eq!(config.programs.command(9), None);
        assert_eq!(config.programs.editor(), None);
    }

    /// Only the nine digits name a program, and `0` is not one of them.
    ///
    /// `0` belongs to the view — it is the key that fits the picture to the
    /// window — and binding a program to it would take a viewing gesture that
    /// has been there since v0.1.0.
    #[test]
    fn only_the_nine_digits_name_a_program() {
        let config = Config::parse("program_0 = C:/zero.exe\nprogram_10 = C:/ten.exe\nprogram_x = C:/x.exe\nprogram_9 = C:/nine.exe");
        assert_eq!(config.programs.command(9), Some(std::path::Path::new("C:/nine.exe")));
        assert!(
            config.programs.commands.iter().filter(|program| !program.as_os_str().is_empty()).count() == 1,
            "a key outside 1-9 was bound to a program",
        );
    }

    /// The editor is empty by default, and empty means "ask Windows".
    ///
    /// This is what makes `E` work on a viewer nobody has configured: an
    /// editor that had to be set before the key did anything would be a
    /// setting standing in for a choice.
    #[test]
    fn the_editor_is_unset_by_default_so_windows_chooses() {
        assert_eq!(Config::parse("").programs.editor(), None);
        assert_eq!(
            Config::parse("editor = C:/Program Files/Paint Something/paint.exe").programs.editor(),
            Some(std::path::Path::new("C:/Program Files/Paint Something/paint.exe")),
        );
    }

    /// The magnifier is on unless the file says otherwise, and only the word
    /// `false` says otherwise — a typo must not quietly shrink the panel.
    #[test]
    fn the_magnifier_is_on_unless_turned_off() {
        assert!(Config::parse("").tools.magnifier);
        assert!(!Config::parse("magnifier = false").tools.magnifier);
        assert!(Config::parse("magnifier = off").tools.magnifier);
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

    /// The backdrop survives the file under its own name, including the
    /// default — which the status line leaves unnamed but the file cannot.
    #[test]
    fn the_backdrop_survives_a_round_trip_including_the_default() {
        for backdrop in Backdrop::ALL {
            let config = Config {
                appearance: Appearance {
                    backdrop,
                    ..Appearance::default()
                },
                ..Config::default()
            };
            assert_eq!(
                Config::parse(&config.render()).appearance.backdrop,
                backdrop,
                "{backdrop:?} did not survive being written and read back"
            );
        }

        // Every keyword is distinct, or two backdrops would read back as one.
        let mut keywords: Vec<&str> = Backdrop::ALL.iter().map(|backdrop| backdrop.keyword()).collect();
        keywords.sort_unstable();
        let before = keywords.len();
        keywords.dedup();
        assert_eq!(keywords.len(), before, "two backdrops share a keyword: {keywords:?}");
    }

    /// A word that names no backdrop falls back rather than refusing to load.
    #[test]
    fn an_unknown_backdrop_falls_back_to_the_default() {
        assert_eq!(Config::parse("backdrop = plaid").appearance.backdrop, Backdrop::default());
    }

    /// Ctrl+wheel is always the gesture the bare wheel is not, so that both
    /// are reachable whichever way round the setting is.
    #[test]
    fn the_modifier_always_offers_the_other_gesture() {
        assert_eq!(Wheel::Zoom.modified(), Wheel::Step);
        assert_eq!(Wheel::Step.modified(), Wheel::Zoom);
    }
}
