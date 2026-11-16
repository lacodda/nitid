//! What the updater remembers: when it last asked, and what came of it.
//!
//! `%LOCALAPPDATA%\lacodda\nitid\update\state`, a few `key = value` lines.
//! Written the way the settings are, but not in them: the settings file belongs
//! to the open viewer, which writes all of it when it closes.
//!
//! Failing to read or write this costs one more question to GitHub at worst, so
//! it never stops anything.

use std::fs;
use std::path::PathBuf;

use super::feed::Version;

/// How the updater left things.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct State {
    /// When GitHub last answered, in seconds since 1970.
    pub checked: Option<u64>,
    /// What it said the newest release was.
    pub latest: Option<Version>,
    /// A version just installed that nobody has been told about yet.
    pub news: Option<Version>,
    /// Why the last attempt to update did not go through.
    pub failed: Option<String>,
    /// A newer release whose download was put off on a metered connection.
    pub held: Option<Version>,
}

impl State {
    pub fn load() -> Self {
        path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// Written beside and moved into place: the resident and an installing
    /// process both read this, and a half-written file reads as nothing.
    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let beside = path.with_extension(format!("{}.new", std::process::id()));
        if fs::write(&beside, self.render()).is_ok() && fs::rename(&beside, &path).is_err() {
            let _ = fs::remove_file(&beside);
        }
    }

    fn parse(text: &str) -> Self {
        let mut state = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "checked" => state.checked = value.parse().ok(),
                "latest" => state.latest = Version::parse(value),
                "news" => state.news = Version::parse(value),
                "held" => state.held = Version::parse(value),
                "failed" => state.failed = (!value.is_empty()).then(|| value.to_string()),
                _ => {}
            }
        }
        state
    }

    fn render(&self) -> String {
        let mut out = String::new();
        if let Some(checked) = self.checked {
            out.push_str(&format!("checked = {checked}\n"));
        }
        if let Some(latest) = self.latest {
            out.push_str(&format!("latest = {latest}\n"));
        }
        if let Some(news) = self.news {
            out.push_str(&format!("news = {news}\n"));
        }
        if let Some(held) = self.held {
            out.push_str(&format!("held = {held}\n"));
        }
        if let Some(failed) = &self.failed {
            // One line: an error from the system can run to several, and the
            // rest would be read as settings.
            out.push_str(&format!("failed = {}\n", failed.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
        out
    }
}

/// The directory the updater works in.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(base).join("lacodda").join("nitid").join("update"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("state"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_written_is_read_back() {
        let state = State {
            checked: Some(1_790_000_000),
            latest: Version::parse("0.43.0"),
            news: Version::parse("0.42.0"),
            failed: Some("the disk is full".to_string()),
            held: Version::parse("0.44.0"),
        };
        assert_eq!(State::parse(&state.render()), state);
        assert_eq!(State::parse(&State::default().render()), State::default());
    }

    /// A message with a line break in it stays one value, and cannot write a
    /// second key.
    #[test]
    fn a_failure_of_several_lines_stays_one() {
        let state = State {
            failed: Some("first line\r\nnews = 9.9.9".to_string()),
            ..State::default()
        };
        let read = State::parse(&state.render());
        assert_eq!(read.news, None, "a line of an error was read as a key");
        assert_eq!(read.failed.as_deref(), Some("first line news = 9.9.9"));
    }

    #[test]
    fn a_damaged_file_reads_as_nothing_known() {
        assert_eq!(State::parse("checked = soon\nlatest = new\nnews\n\u{fffd}"), State::default());
    }
}
