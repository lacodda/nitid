//! What the viewer remembers between runs.
//!
//! Only the window placement for now. It lives in `%APPDATA%\lacodda\nitid`,
//! written as a handful of `key = value` lines: a viewer that needs a TOML
//! parser to remember where its window was is carrying a dependency for
//! nothing, and the file stays readable to anyone who opens it.
//!
//! Every failure here is silent. A viewer that refuses to open because its
//! settings file is unreadable has its priorities backwards; the defaults are
//! always a usable answer.

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

/// The settings as they stand.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Config {
    pub placement: Placement,
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
                _ => {}
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

        out
    }
}

fn parse_pair(value: &str) -> Option<(i32, i32)> {
    let (first, second) = value.split_once(',')?;
    Some((first.trim().parse().ok()?, second.trim().parse().ok()?))
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
    }

    #[test]
    fn a_placement_survives_a_round_trip() {
        let config = Config {
            placement: Placement {
                position: Some((120, -40)),
                size: Some((1600, 900)),
                maximised: false,
            },
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
             something_else = 5\n\
             malformed line without a separator\n",
        );
        assert_eq!(config.placement.position, None);
        // A zero size would open a window with nothing in it.
        assert_eq!(config.placement.size, None);
    }

    #[test]
    fn unknown_keys_do_not_disturb_known_ones() {
        let config = Config::parse("future_setting = 1\nwindow_size = 800, 600\n");
        assert_eq!(config.placement.size, Some((800, 600)));
    }
}
