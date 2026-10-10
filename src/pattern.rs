//! A name pattern: which pictures of a folder are walked, and which ones the
//! desktop picture is chosen from.
//!
//! The patterns are the ones Windows itself speaks — `*.png`, `IMG_????.jpg` —
//! because those are what a person already types into a file dialog. Three
//! rules, and nothing a person would have to look up:
//!
//! - `*` stands for any run of characters and `?` for exactly one;
//! - several patterns are separated by `;`, and a name matching any of them
//!   is admitted, the way a file dialog's filter reads `*.jpg;*.png`;
//! - a pattern with no wildcard in it matches anywhere in the name, so typing
//!   `final` finds `cat_final_v2.png` — what typing a word into Explorer's
//!   search box does.
//!
//! Matching ignores case, as the file system it is matched against does.

use std::path::Path;

/// A pattern, parsed once and matched against many names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    /// The pattern as it was typed, tidied: what the status line shows and
    /// what the settings file keeps.
    text: String,
    /// Each alternative, lowercased, with a bare word already turned into
    /// `*word*`.
    alternatives: Vec<Vec<char>>,
}

impl Pattern {
    /// Read a pattern as typed. `None` when there is nothing in it — an empty
    /// box means "no pattern", and the caller lifts the filter.
    pub fn parse(text: &str) -> Option<Self> {
        let typed: Vec<&str> = text.split(';').map(str::trim).filter(|alternative| !alternative.is_empty()).collect();
        if typed.is_empty() {
            return None;
        }

        let alternatives = typed
            .iter()
            .map(|alternative| {
                let lowered = alternative.to_lowercase();
                if lowered.contains(['*', '?']) {
                    lowered.chars().collect()
                } else {
                    format!("*{lowered}*").chars().collect()
                }
            })
            .collect();

        Some(Self {
            text: typed.join(";"),
            alternatives,
        })
    }

    /// The pattern as typed, for showing back.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether a file's name is one this pattern admits.
    ///
    /// The name only, never the folders above it: `*.png` is a question about
    /// what a file is called, and a folder named `drafts.png` must not let
    /// every picture inside it through.
    pub fn admits(&self, path: &Path) -> bool {
        let Some(name) = path.file_name() else {
            return false;
        };
        self.matches(&name.to_string_lossy())
    }

    /// Whether a bare name matches.
    pub fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.to_lowercase().chars().collect();
        self.alternatives.iter().any(|alternative| glob(alternative, &name))
    }
}

/// `*` and `?` against a name, by walking both once and remembering the last
/// star to fall back to.
///
/// The fall-back is what makes `*a*b` find `xaxxb`: when a run after a star
/// stops matching, the star takes one more character and the run starts
/// again. Only the last star needs remembering, because everything before it
/// already matched as early as it could.
fn glob(pattern: &[char], name: &[char]) -> bool {
    let (mut at_pattern, mut at_name) = (0, 0);
    let mut star: Option<(usize, usize)> = None;

    while at_name < name.len() {
        match pattern.get(at_pattern) {
            Some('*') => {
                star = Some((at_pattern, at_name));
                at_pattern += 1;
            }
            Some(&wanted) if wanted == '?' || wanted == name[at_name] => {
                at_pattern += 1;
                at_name += 1;
            }
            _ => match star {
                Some((star_at, taken)) => {
                    at_pattern = star_at + 1;
                    at_name = taken + 1;
                    star = Some((star_at, taken + 1));
                }
                None => return false,
            },
        }
    }

    // The name is used up: whatever is left of the pattern has to be stars,
    // which match nothing as happily as anything.
    pattern[at_pattern..].iter().all(|&character| character == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admits(pattern: &str, name: &str) -> bool {
        Pattern::parse(pattern).expect("a pattern").matches(name)
    }

    #[test]
    fn a_star_stands_for_any_run_of_characters() {
        assert!(admits("*.png", "cat.png"));
        assert!(admits("*.png", ".png"), "a star matches nothing too");
        assert!(!admits("*.png", "cat.png.jpg"), "the pattern has to reach the end of the name");
        assert!(!admits("*.png", "cat.jpg"));
        assert!(admits("ComfyUI_*_.png", "ComfyUI_00012_.png"));
    }

    #[test]
    fn a_question_mark_stands_for_exactly_one() {
        assert!(admits("IMG_????.jpg", "IMG_0042.jpg"));
        assert!(!admits("IMG_????.jpg", "IMG_042.jpg"), "three characters stood in for four");
        assert!(!admits("IMG_????.jpg", "IMG_00042.jpg"), "five characters stood in for four");
    }

    /// `*ab` against `aab` is the case a matcher without the fall-back gets
    /// wrong: the first `a` matches, the second is not a `b`, and only by
    /// letting the star take that first `a` does the run match from the
    /// second one.
    #[test]
    fn a_run_after_a_star_that_stops_matching_starts_again_further_on() {
        assert!(admits("*ab", "aab"));
        assert!(admits("*a*b", "xaxxb"));
        assert!(admits("*_final.png", "cat_final_final.png"));
        assert!(!admits("*_final.png", "cat_final.png.bak"));
    }

    #[test]
    fn case_is_ignored_as_the_file_system_ignores_it() {
        assert!(admits("*.PNG", "cat.png"));
        assert!(admits("*.png", "CAT.PNG"));
        assert!(admits("*.jpg", "ФОТО.JPG"), "the case of a name outside ASCII is ignored too");
    }

    #[test]
    fn a_bare_word_matches_anywhere_in_the_name() {
        assert!(admits("final", "cat_final_v2.png"));
        assert!(admits("FINAL", "cat_final_v2.png"));
        assert!(!admits("final", "cat_draft.png"));
    }

    #[test]
    fn alternatives_admit_a_name_that_matches_any_of_them() {
        let pattern = Pattern::parse(" *.jpg ; *.png ;; ").expect("a pattern");
        assert!(pattern.matches("a.jpg"));
        assert!(pattern.matches("a.png"));
        assert!(!pattern.matches("a.webp"));
        // Tidied for showing back: the empty alternative and the spaces go.
        assert_eq!(pattern.text(), "*.jpg;*.png");
    }

    #[test]
    fn an_empty_pattern_is_no_pattern() {
        assert_eq!(Pattern::parse(""), None);
        assert_eq!(Pattern::parse("  ;  ; "), None);
    }

    /// The name, never the folder: a folder called `drafts.png` must not let
    /// every picture inside it through.
    #[test]
    fn only_the_name_is_matched_not_the_folders_above_it() {
        let pattern = Pattern::parse("*.png").expect("a pattern");
        assert!(!pattern.admits(Path::new("drafts.png").join("cat.jpg").as_path()));
        assert!(pattern.admits(Path::new("drafts").join("cat.png").as_path()));
    }
}
