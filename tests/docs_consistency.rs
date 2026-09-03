//! The gate that keeps the README honest about the product.
//!
//! Two lists say what the keys are: `interface::KEYS`, which the key sheet
//! draws, and the table in the README. Nothing held them together, and a
//! version that adds a key touches one of them — v0.23.0 added `Ctrl+Drag`
//! to both by hand and only noticed because the work happened to pass through
//! both files. The next one would not be so lucky.
//!
//! The same for the formats: the table in the README and `Format::ALL`, which
//! is what the installer registers and what the viewer opens.
//!
//! This is a slice of the `release_consistency` test v0.40.0 is for, brought
//! forward because the seam is open now.

use std::collections::BTreeSet;

/// The README, read from the source tree rather than embedded, so a failure
/// names the file the author has to fix.
fn readme() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md")).expect("README.md is missing")
}

/// Strip the markdown a table cell wraps a key in.
///
/// The README writes `` `←` `→` `` and the key sheet writes `← →`: the same
/// keys, spelled for two different readers. Comparing them raw would fail on
/// punctuation and teach the author to weaken the test rather than fix the
/// list, so both sides are reduced to the keys themselves.
fn normalise(cell: &str) -> String {
    cell.replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every key the viewer answers is in the README, and every key the README
/// promises is one the viewer answers.
#[test]
fn the_readme_lists_the_keys_the_viewer_answers() {
    let readme = readme();
    let sheet: BTreeSet<String> = nitid::testing::keys().iter().map(|(key, _)| normalise(key)).collect();

    // The keys table is the one whose header is "Key | Action".
    let table = readme
        .split("| Key | Action |")
        .nth(1)
        .expect("the README has no keys table")
        .lines()
        // `split` leaves the tail of the header line first, so the rows do not
        // start until the line after it — skipping by count got this wrong in
        // both directions, so the rule under the header is what starts the
        // table and the first line that is not a row ends it.
        .skip_while(|line| !line.trim_start_matches(['|', ' ']).starts_with("---"))
        .skip(1)
        .take_while(|line| line.starts_with('|'));

    let documented: BTreeSet<String> = table
        .filter_map(|line| line.split('|').nth(1).map(normalise))
        .filter(|cell| !cell.is_empty())
        .collect();

    let missing: Vec<&String> = sheet.difference(&documented).collect();
    assert!(missing.is_empty(), "the key sheet answers these and the README does not list them: {missing:?}");

    let invented: Vec<&String> = documented.difference(&sheet).collect();
    assert!(
        invented.is_empty(),
        "the README promises these and the viewer does not answer them: {invented:?}"
    );
}

/// Every extension the installer registers appears in the README's format
/// table, so a format added to the code cannot arrive undocumented.
#[test]
fn the_readme_lists_the_formats_the_viewer_opens() {
    let readme = readme();
    let registered = nitid::testing::extensions();

    let undocumented: Vec<&str> = registered
        .iter()
        .copied()
        .filter(|extension| !readme.contains(&format!("`.{extension}`")))
        .collect();

    assert!(
        undocumented.is_empty(),
        "these extensions open but the README does not mention them: {undocumented:?}"
    );
}

/// The version in the manifest is the one the README's status line names.
///
/// The status paragraph opens with "vX.Y.Z is out", which is the first thing a
/// visitor reads and the easiest thing to leave behind at release time.
#[test]
fn the_readme_names_the_version_in_the_manifest() {
    let version = env!("CARGO_PKG_VERSION");
    let readme = readme();
    assert!(
        readme.contains(&format!("v{version} is out")),
        "the README's status line does not say v{version} is out",
    );
}
