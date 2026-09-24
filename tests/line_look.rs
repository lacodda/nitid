//! The gate that keeps nitid dressed as a product of the line.
//!
//! The interface's colours come from dowel: `assets/dowel/palette.json` is
//! every colour of the line's theme resolved for nitid's accent, `build.rs`
//! turns it into constants, and `src/theme.rs` hands them out. That only holds
//! while nobody types a colour into the interface by hand — one `from_rgb`
//! with a grey that looked right on the day is where a product starts to drift
//! from the line again, and it would pass every other test.
//!
//! So the interface is read as text, and a colour written as numbers there
//! fails. The exceptions (marks drawn on the photograph, the histogram's
//! channels, the file's own pixels) are not exceptions to this rule: they live
//! in `theme.rs` under names that say why, and the interface uses the names.

/// A source file, read from the tree so a failure names the file to fix.
fn source(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// What a colour written by hand looks like in egui: an associated function
/// or constant of one of its colour types.
const TYPED_COLOUR: [&str; 4] = ["Color32::", "Rgba::", "Hsva", "ecolor::"];

/// The lines of `text` that write a colour, with their line numbers.
///
/// Comments are skipped, so a sentence explaining why a colour is not typed
/// here does not trip the gate that says it must not be.
fn typed_colours(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter(|(_, line)| TYPED_COLOUR.iter().any(|pattern| line.contains(pattern)))
        .map(|(index, line)| (index + 1, line.trim().to_owned()))
        .collect()
}

/// The interface without its tests: a test is allowed to name the exact
/// colour it expects to find painted.
fn interface_code() -> String {
    let text = source("src/interface.rs");
    let marker = "\n#[cfg(test)]\nmod tests";
    let end = text
        .find(marker)
        .expect("src/interface.rs no longer has its test module where this gate looks for it");
    text[..end].to_owned()
}

#[test]
fn the_interface_writes_no_colour_by_hand() {
    let found = typed_colours(&interface_code());
    assert!(
        found.is_empty(),
        "src/interface.rs writes these colours itself; take them from `crate::theme` \
         (a dowel token, or a named exception there with its reason):\n{}",
        found.iter().map(|(line, text)| format!("  {line}: {text}")).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn the_gate_sees_a_colour_when_there_is_one() {
    // A gate that reads source can go blind without going red: a pattern that
    // stops matching finds nothing, and nothing is exactly what it wants to
    // find. So it is shown colours it must see — a sample, and the module
    // that is allowed to hold them — and must find them.
    let sample = "    let fill = egui::Color32::from_rgb(14, 16, 20);\n    // egui::Color32::WHITE in a comment\n";
    assert_eq!(typed_colours(sample), [(1, "let fill = egui::Color32::from_rgb(14, 16, 20);".to_owned())]);
    assert!(
        typed_colours(&source("src/theme.rs")).len() >= 5,
        "the gate finds almost no colours in theme.rs, which holds all of them — it has stopped seeing"
    );
    // And the part of the interface it reads is the whole of it rather than a
    // stub: the toolbar and the status line are both in there.
    let code = interface_code();
    assert!(code.contains("fn toolbar(") && code.contains("fn status_line("));
}

#[test]
fn the_tokens_come_from_one_release() {
    // Both files are copied from the same `dowel-ui` release; CI compares them
    // with it byte for byte. Here only that the palette is nitid's and says
    // which release it is, so a hand-edited or foreign file cannot pass for
    // one.
    let palette = source("assets/dowel/palette.json");
    assert!(palette.contains("\"product\": \"nitid\""), "assets/dowel/palette.json is not nitid's palette");
    assert!(palette.contains("\"package\": \"dowel-ui\""));
    assert!(
        palette.contains("\"version\": \""),
        "the palette does not say which dowel-ui release it came from"
    );
    assert!(source("assets/dowel/tokens.json").contains("\"radius\""));
}
