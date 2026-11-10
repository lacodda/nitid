//! What a release says about itself, and where to find it.
//!
//! One small text file sits beside every release's zip, and the address
//! `releases/latest/download/update.txt` always answers with the newest
//! published one. A draft is not served there, so a release is offered to
//! nobody until it has been checked on a real machine and published.
//!
//! A file rather than the tag the `latest` redirect names, because every copy
//! out there will go on reading this for as long as it runs and none of them
//! can be corrected afterwards: a line this version does not know is ignored,
//! so a later release can say more without the earlier ones tripping on it.
//! The format is written by `.github/workflows/release.yml`.

use std::fmt;

use anyhow::{Context, Result, bail};

/// The file every release carries.
pub const MANIFEST: &str = "update.txt";

/// Where releases are, unless a test says otherwise.
const RELEASES: &str = "https://github.com/lacodda/nitid/releases";

/// The largest archive a release may name. The real one is about eighteen
/// megabytes (v0.42), and a bad line must not have this process filling the
/// disk.
const LARGEST_ARCHIVE: u64 = 256 * 1024 * 1024;

/// A release's number. Ordered by its parts, as numbers: 0.9.0 is before
/// 0.10.0, which comparing the text would get wrong.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Version(u32, u32, u32);

impl Version {
    /// `1.2.3` and nothing looser: this decides what gets installed, so a
    /// suffix or a missing part is a refusal rather than a best guess.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(|part| {
            if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse::<u32>().ok()
            } else {
                None
            }
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }

    /// The version this program is.
    ///
    /// `NITID_UPDATE_CURRENT` has the updater believe it is older than it is,
    /// which is how a gate and a rehearsal install a real release over a copy
    /// of itself. It changes what the updater thinks and nothing `--version`
    /// says.
    pub fn current() -> Self {
        std::env::var("NITID_UPDATE_CURRENT")
            .ok()
            .and_then(|text| Self::parse(&text))
            .unwrap_or_else(Self::built)
    }

    /// The version in this program's manifest, whatever the updater is told.
    pub fn built() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).expect("the manifest's version is three numbers")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// What the manifest says about a release.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Release {
    pub version: Version,
    /// The zip's file name. Only ever a name: where it is fetched from is
    /// this program's to say (see [`Feed::archive`]).
    pub archive: String,
    pub sha256: [u8; 32],
    pub size: u64,
}

impl Release {
    pub fn parse(text: &str) -> Result<Self> {
        let mut version = None;
        let mut archive = None;
        let mut sha256 = None;
        let mut size = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "version" => version = Some(Version::parse(value).with_context(|| format!("the version `{value}` is not three numbers"))?),
                "archive" => archive = Some(archive_name(value)?),
                "sha256" => sha256 = Some(digest(value).with_context(|| format!("the digest `{value}` is not 64 hexadecimal digits"))?),
                "size" => size = Some(value.parse::<u64>().with_context(|| format!("the size `{value}` is not a number"))?),
                // Something a later release says that this one has no use for.
                _ => {}
            }
        }
        let size = size.context("no size")?;
        if size == 0 || size > LARGEST_ARCHIVE {
            bail!("an archive of {size} bytes is not one this program takes");
        }
        Ok(Self {
            version: version.context("no version")?,
            archive: archive.context("no archive")?,
            sha256: sha256.context("no digest")?,
            size,
        })
    }
}

/// A file name, and nothing that could make it a path or leave the address.
fn archive_name(name: &str) -> Result<String> {
    let plain = name.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if name.is_empty() || !plain || name.starts_with('.') {
        bail!("`{name}` is not a file name");
    }
    Ok(name.to_string())
}

fn digest(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (at, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Where the releases are.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Feed {
    base: String,
}

impl Feed {
    /// The real one, or what `NITID_UPDATE_FEED` names for a gate.
    ///
    /// Only this machine can stand in for GitHub. A variable that could name
    /// any address would be a way to have this program install what it finds
    /// there, and the one place it is needed is a test with a server of its
    /// own.
    pub fn new() -> Result<Self> {
        match std::env::var("NITID_UPDATE_FEED") {
            Ok(base) => Self::standing_in(&base),
            Err(_) => Ok(Self { base: RELEASES.to_string() }),
        }
    }

    fn standing_in(base: &str) -> Result<Self> {
        if !base.starts_with("http://127.0.0.1:") {
            bail!("NITID_UPDATE_FEED names {base}, and only this machine can stand in for the releases");
        }
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
        })
    }

    /// Whether a test is standing in for GitHub.
    pub fn stood_in_for() -> bool {
        std::env::var_os("NITID_UPDATE_FEED").is_some()
    }

    /// Always the newest published release's manifest.
    pub fn latest(&self) -> String {
        format!("{}/latest/download/{MANIFEST}", self.base)
    }

    /// Where a release's zip is. Built here from the version and the name
    /// rather than taken from the manifest, so the file cannot send this
    /// program to another address.
    pub fn archive(&self, release: &Release) -> String {
        format!("{}/download/v{}/{}", self.base, release.version, release.archive)
    }

    /// What a person reads about a release.
    pub fn notes(version: Version) -> String {
        format!("{RELEASES}/tag/v{version}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE: &str = "version = 0.43.0\n\
        archive = nitid-v0.43.0-x86_64-pc-windows-msvc.zip\n\
        sha256 = 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\n\
        size = 1234\n";

    /// The order is numeric, part by part. Comparing as text would call
    /// 0.9.0 newer than 0.10.0, and every copy would stop updating at 0.9.
    #[test]
    fn versions_are_ordered_as_numbers() {
        let version = |text| Version::parse(text).expect("a version");
        assert!(version("0.10.0") > version("0.9.0"));
        assert!(version("0.42.1") > version("0.42.0"));
        assert!(version("1.0.0") > version("0.99.99"));
        assert_eq!(version("0.42.0"), version("0.42.0"));
    }

    #[test]
    fn a_version_is_three_numbers_and_nothing_else() {
        for wrong in ["", "1", "1.2", "1.2.3.4", "v1.2.3", "1.2.x", "1.2.3-rc1", "1..3", "+1.2.3", " 1.2.3"] {
            assert_eq!(Version::parse(wrong), None, "`{wrong}` was taken for a version");
        }
        assert_eq!(Version::parse("1.2.3").map(|version| version.to_string()).as_deref(), Some("1.2.3"));
    }

    /// The manifest as the release workflow writes it, with Windows line
    /// endings as well: it is written by bash on a Windows runner.
    #[test]
    fn a_manifest_reads_whatever_its_line_endings() {
        for text in [WHOLE.to_string(), WHOLE.replace('\n', "\r\n")] {
            let release = Release::parse(&text).expect("a whole manifest");
            assert_eq!(release.version, Version(0, 43, 0));
            assert_eq!(release.archive, "nitid-v0.43.0-x86_64-pc-windows-msvc.zip");
            assert_eq!(release.size, 1234);
            assert_eq!((release.sha256[0], release.sha256[31]), (0x00, 0xff));
        }
    }

    /// This copy has to keep working when a later release says more.
    #[test]
    fn a_line_from_a_later_release_is_passed_over() {
        let text = format!("{WHOLE}minimum_windows = 26100\nnot a line at all\n");
        assert!(Release::parse(&text).is_ok());
    }

    #[test]
    fn a_manifest_missing_anything_is_refused() {
        for key in ["version", "archive", "sha256", "size"] {
            let text: String = WHOLE.lines().filter(|line| !line.starts_with(key)).map(|line| format!("{line}\n")).collect();
            assert!(Release::parse(&text).is_err(), "a manifest with no {key} was taken");
        }
    }

    #[test]
    fn a_manifest_with_a_wrong_value_is_refused() {
        for (line, wrong) in [
            ("version = 0.43.0", "version = latest"),
            ("sha256 = 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff", "sha256 = 0011"),
            (
                "sha256 = 00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                "sha256 = zz112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            ),
            ("size = 1234", "size = many"),
            ("size = 1234", "size = 0"),
            ("size = 1234", "size = 999999999999"),
        ] {
            assert!(Release::parse(&WHOLE.replace(line, wrong)).is_err(), "`{wrong}` was taken");
        }
    }

    /// The name goes on the end of an address and on the end of a path, so
    /// it must be unable to leave either.
    #[test]
    fn an_archive_is_a_name_and_not_a_path() {
        for wrong in [
            "../evil.zip",
            r"..\evil.zip",
            "a/b.zip",
            "https://elsewhere/x.zip",
            "x.zip?y",
            "x y.zip",
            ".hidden",
            "",
        ] {
            assert!(archive_name(wrong).is_err(), "`{wrong}` was taken for a name");
        }
        assert!(archive_name("nitid-v0.43.0-x86_64-pc-windows-msvc.zip").is_ok());
    }

    #[test]
    fn the_addresses_are_the_ones_github_answers() {
        let feed = Feed { base: RELEASES.to_string() };
        assert_eq!(feed.latest(), "https://github.com/lacodda/nitid/releases/latest/download/update.txt");
        let release = Release::parse(WHOLE).expect("a whole manifest");
        assert_eq!(
            feed.archive(&release),
            "https://github.com/lacodda/nitid/releases/download/v0.43.0/nitid-v0.43.0-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(Feed::notes(release.version), "https://github.com/lacodda/nitid/releases/tag/v0.43.0");
    }

    /// Nothing but this machine can stand in: a variable naming another
    /// address would be a way to have this program install what it finds
    /// there.
    #[test]
    fn only_this_machine_can_stand_in_for_the_releases() {
        for wrong in [
            "https://github.com/someone/else/releases",
            "http://evil.example/releases",
            "http://127.0.0.1.evil.example/",
            "http://localhost:80/",
            "",
        ] {
            assert!(Feed::standing_in(wrong).is_err(), "`{wrong}` was let stand in");
        }
        let feed = Feed::standing_in("http://127.0.0.1:8123/").expect("this machine");
        assert_eq!(feed.latest(), "http://127.0.0.1:8123/latest/download/update.txt");
    }
}
