//! The gate that keeps the documentation honest about the product.
//!
//! Two lists say what the keys are: `interface::KEYS`, which the key sheet
//! draws, and the table a reader is given. Nothing held them together, and a
//! version that adds a key touches one of them — v0.23.0 added `Ctrl+Drag`
//! to both by hand and only noticed because the work happened to pass through
//! both files. The next one would not be so lucky.
//!
//! The same for the formats: the table and `Format::ALL`, which is what the
//! installer registers and what the viewer opens.
//!
//! Those tables used to live in the README and now live on the documentation
//! site, because a README is a shopfront rather than a reference. The gate
//! followed them: pointing it at the README after the move would have left it
//! passing on text that is no longer there to be wrong.
//!
//! What has to agree before a release goes out is `release_consistency`'s.

use std::collections::BTreeSet;

/// One documentation page, read from the source tree rather than embedded, so
/// a failure names the file the author has to fix.
///
/// With its line endings made `\n`: the Windows runner checks the tree out
/// with `\r\n`, and a page split on a fence followed by `\n` finds no block
/// there at all — the gate passed on every machine but the one the release
/// is built on.
fn page(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
        .replace("\r\n", "\n")
}

/// The page listing every key, and the one listing every format.
fn keys_page() -> String {
    page("docs/src/content/docs/reference/keys.md")
}

fn formats_page() -> String {
    page("docs/src/content/docs/reference/formats.md")
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

/// Every key the viewer answers is on the reference page, and every key that
/// page promises is one the viewer answers.
#[test]
fn the_reference_lists_the_keys_the_viewer_answers() {
    let readme = keys_page();
    let sheet: BTreeSet<String> = nitid::testing::keys().iter().map(|(key, _)| normalise(key)).collect();

    // The keys table is the one whose header is "Key | Action".
    let table = readme
        .split("| Key | Action |")
        .nth(1)
        .expect("the keys reference page has no keys table")
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
    assert!(
        missing.is_empty(),
        "the key sheet answers these and the keys reference does not list them: {missing:?}"
    );

    let invented: Vec<&String> = documented.difference(&sheet).collect();
    assert!(
        invented.is_empty(),
        "the keys reference promises these and the viewer does not answer them: {invented:?}"
    );
}

/// Every extension the installer registers appears in the reference's format
/// table, so a format added to the code cannot arrive undocumented.
#[test]
fn the_reference_lists_the_formats_the_viewer_opens() {
    let readme = formats_page();
    let registered = nitid::testing::extensions();

    let undocumented: Vec<&str> = registered
        .iter()
        .copied()
        .filter(|extension| !readme.contains(&format!("`.{extension}`")))
        .collect();

    assert!(
        undocumented.is_empty(),
        "these extensions open but the formats reference does not mention them: {undocumented:?}"
    );
}

fn command_line_page() -> String {
    page("docs/src/content/docs/reference/command-line.md")
}

/// The command-line page shows what `--help` says the commands are: every
/// line of the synopsis, word for word, and every option `--help` names.
///
/// The same arrangement the keys table has with the key sheet. A command or
/// a flag added to the program and not to the page fails here, and so does
/// one the page still promises after the program dropped it — the page's
/// synopsis is held to `--help` line by line in both directions.
#[test]
fn the_command_line_page_shows_what_help_says() {
    let usage = nitid::testing::usage();
    let page = command_line_page();

    let synopsis: Vec<&str> = usage
        .split("Usage:")
        .nth(1)
        .expect("--help has no usage section")
        .lines()
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .map(str::trim)
        .collect();
    assert!(!synopsis.is_empty(), "--help lists no commands");

    let block: Vec<&str> = page
        .split("```\n")
        .nth(1)
        .expect("the command-line page opens with no synopsis block")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    for line in &synopsis {
        assert!(block.contains(line), "--help lists `{line}` and the page's synopsis does not");
    }
    for line in block.iter().filter(|line| !line.ends_with("--help") && !line.ends_with("--version")) {
        assert!(synopsis.contains(line), "the page's synopsis promises `{line}` and --help does not list it");
    }

    let options: BTreeSet<&str> = usage
        .split(|character: char| character.is_whitespace() || matches!(character, '[' | ']' | ',' | '(' | ')'))
        .filter(|word| word.starts_with("--") && word.len() > 2)
        .collect();
    let undocumented: Vec<&&str> = options
        .iter()
        .filter(|option| !page.contains(&format!("`{option}")) && !block.iter().any(|line| line.contains(*option)))
        .collect();
    assert!(undocumented.is_empty(), "--help names these options and the page does not: {undocumented:?}");
}

/// The page about Explorer names every extension nitid draws thumbnails for
/// and no other, so a format that joins them cannot arrive undocumented and one
/// the page promises is one that comes.
#[test]
fn the_explorer_page_lists_what_gets_a_thumbnail() {
    let page = page("docs/src/content/docs/guides/working-from-explorer.md");
    let section = page
        .split("## Thumbnails")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .expect("the page has no section about thumbnails");
    let promised: BTreeSet<&str> = section
        .split('`')
        .filter(|code| code.len() > 1 && code.starts_with('.') && code[1..].chars().all(char::is_alphanumeric))
        .map(|code| &code[1..])
        .collect();
    let drawn: BTreeSet<&str> = nitid::testing::thumbnail_extensions().into_iter().collect();
    assert_eq!(promised, drawn);
}
