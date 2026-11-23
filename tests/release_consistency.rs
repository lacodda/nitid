//! Guards the facts that have to agree before a release goes out, and that are
//! nitid's own.
//!
//! What every program of the line has to agree on - one version in the
//! manifests, the changelog's section, one description, a README whose links
//! work off GitHub, an installer that names the right files - is held by `lyrn
//! check`, in the Shopfronts workflow on every push. What is left here is what
//! only nitid has: two programs in one zip, an updater in every copy out there
//! that cannot be corrected, and a rehearsal that reads the registry.
//!
//! A mistake in any of these shows after the tag, in a workflow that stops
//! halfway or - the one this is really for - on somebody's machine that can no
//! longer be updated. Here it stops a commit.

use std::collections::BTreeSet;
use std::path::Path;

/// One file of the repository, with its line endings made `\n`: the Windows
/// runner checks the tree out with `\r\n`.
fn read(relative: &str) -> String {
    let path = format!("{}/{relative}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
        .replace("\r\n", "\n")
}

/// Every workflow, by its file's name.
fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| entry.expect("a workflow").file_name().to_string_lossy().into_owned())
        .map(|name| (name.clone(), read(&format!(".github/workflows/{name}"))))
        .collect();
    found.sort();
    assert!(found.len() > 3, "the workflows were not found: {found:?}");
    found
}

/// The README's status line opens with the version, which is the first thing a
/// visitor checks and the easiest thing to leave behind at release time.
#[test]
fn the_readme_names_the_version_in_the_manifest() {
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        read("README.md").contains(&format!("v{version}")),
        "the README's status line does not name v{version}"
    );
}

/// The minimum Rust is a promise to whoever builds it, and CONTRIBUTING is
/// where they read it. CI holds the manifest's number to the code.
#[test]
fn contributing_names_the_rust_the_manifest_promises() {
    let promised = env!("CARGO_PKG_RUST_VERSION");
    let text = read("CONTRIBUTING.md");
    assert!(
        text.contains(&format!("Requires Rust {promised} or newer")),
        "CONTRIBUTING.md does not say Rust {promised}, which Cargo.toml does"
    );
}

/// The command the installer says it is run with, from its own header.
fn install_command() -> String {
    let script = read("tools/install.ps1");
    let mut lines = script.lines();
    lines
        .find(|line| line.starts_with("# nitid installer for Windows:"))
        .and_then(|_| lines.next())
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .expect("tools/install.ps1 has no `installer for Windows:` header naming its command")
}

/// Installing is one command (owner's decision, 2026-10-07): the README and the
/// getting-started page are where a person copies it from, so both carry
/// exactly the command the script says it is run with, and neither offers a
/// second way in that the script knows nothing about.
#[test]
fn nitid_is_installed_by_the_script_and_only_by_it() {
    let command = install_command();
    assert_eq!(command, "irm https://raw.githubusercontent.com/lacodda/nitid/main/tools/install.ps1 | iex");

    for relative in ["README.md", "docs/src/content/docs/getting-started.md"] {
        let text = read(relative);
        assert!(text.contains(&command), "{relative} does not show `{command}`");
        for other in ["Download the zip", "releases page", ".msi", "cargo install", "winget", "scoop install"] {
            assert!(!text.contains(other), "{relative} mentions `{other}`, a way to install that the script is not");
        }
    }
}

/// The release is packed with exactly what the script fetches and runs: an
/// asset renamed in one place is an installer that answers 404.
#[test]
fn the_release_carries_what_the_script_downloads() {
    let workflow = read(".github/workflows/release.yml");
    assert!(
        workflow.contains(r#"7z a "nitid-${TAG}-x86_64-pc-windows-msvc.zip""#),
        "the release no longer packs the zip the installer downloads"
    );

    let script = read("tools/install.ps1");
    assert!(script.contains(r#"$name = "nitid-$tag-x86_64-pc-windows-msvc.zip""#));
    assert!(
        script.contains("& $exe.FullName install"),
        "the script no longer runs `nitid install` from the release"
    );
}

/// The programs `Cargo.toml` builds are the programs the zip carries and the
/// programs an install puts in place - **two, by these names, for good**.
///
/// A copy out there unpacks the programs it knows by name and starts the second
/// of them. One more that an install depended on would never reach it, and it
/// could not be told: whatever else an install needs travels inside these two.
#[test]
fn a_release_is_two_programs() {
    let programs = ["nitid.exe", "nitidw.exe"];

    let mut built: Vec<String> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin"))
        .expect("the binaries")
        .map(|entry| entry.expect("a binary").path())
        .map(|path| format!("{}.exe", path.file_stem().expect("a name").to_string_lossy()))
        .collect();
    built.sort();
    assert_eq!(built, programs, "Cargo builds other programs than the release is made of");

    let workflow = read(".github/workflows/release.yml");
    let packed: Vec<&str> = workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("cp target/release/"))
        .flat_map(|line| line.split_whitespace())
        .filter_map(|word| word.strip_prefix("target/release/"))
        .collect();
    assert_eq!(packed, programs, "the zip carries other programs than an install puts in place");

    #[cfg(windows)]
    assert_eq!(nitid::testing::executables(), programs, "an install puts other programs in place");
}

/// What every copy already out there was built to do, word for word. It asks
/// for this file, at this address, for these four keys, and starts what it
/// fetched with this argument; none of those copies can be corrected (ADR
/// 0043).
#[test]
fn every_copy_out_there_can_still_update() {
    let workflow = read(".github/workflows/release.yml");
    for line in [
        r#"echo "version = ${TAG#v}""#,
        r#"echo "archive = $archive""#,
        r#"echo "sha256 = $(sha256sum "$archive" | cut -d' ' -f1)""#,
        r#"echo "size = $(stat -c %s "$archive")""#,
        r#"archive="nitid-${TAG}-x86_64-pc-windows-msvc.zip""#,
        "} > update.txt",
    ] {
        assert!(workflow.contains(line), "the release no longer writes `{line}` for the updater");
    }

    let feed = read("src/update/feed.rs");
    assert!(
        feed.contains("latest/download/update.txt"),
        "the updater asks for another manifest than the ones before it"
    );
    let arguments = read("src/lib.rs");
    assert!(
        arguments.contains(r#"const APPLY_UPDATE_ARGUMENT: &str = "--apply-update";"#),
        "a new version no longer answers to the argument old ones start it with"
    );
}

/// `release.toml` is what `furca release run` believes about a release; the
/// workflow is what one is. Each file the page has to carry is one the workflow
/// uploads, and each workflow it waits for exists.
#[test]
fn the_release_engine_is_told_what_the_workflow_does() {
    let engine = read("release.toml");
    let workflow = read(".github/workflows/release.yml");

    let assets: Vec<String> = engine
        .split("assets = [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .expect("release.toml names no assets")
        .lines()
        .map(|line| line.trim().trim_end_matches(',').trim_matches('"').replace("{tag}", "${TAG}"))
        .filter(|line| !line.is_empty())
        .collect();
    assert!(!assets.is_empty());
    for asset in &assets {
        assert!(
            workflow.contains(asset.as_str()),
            "release.toml expects `{asset}` and the workflow makes no such file"
        );
    }
    let uploaded = workflow
        .split("files: |\n")
        .nth(1)
        .expect("the workflow uploads nothing")
        .lines()
        .take_while(|line| line.starts_with("            "))
        .count();
    assert_eq!(
        uploaded,
        assets.len(),
        "the workflow uploads a different number of files than release.toml expects"
    );

    for key in ["workflow = \"", "publish = \""] {
        let named = engine
            .split(key)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_else(|| panic!("release.toml has no `{key}`"));
        read(&format!(".github/workflows/{named}"));
    }
}

/// Nothing is published that the two clean machines have not taken first.
#[test]
fn a_release_is_rehearsed_before_it_is_published() {
    let workflow = read(".github/workflows/release.yml");
    assert!(workflow.contains("draft: true"), "the release is not drafted");
    assert!(
        workflow.contains("needs: [installs, updates]"),
        "the release is published without its rehearsal"
    );
    assert!(
        workflow.contains("--draft=false --latest"),
        "nothing publishes the release, and the updater never sees it"
    );
}

/// The rehearsal looks for what an install writes, by name, on a machine after
/// the tag. A key renamed in the program alone would fail it there.
#[test]
fn the_rehearsal_asks_for_what_an_install_writes() {
    let program = read("src/install.rs");
    let written = [
        (r#"const PROGID: &str = "lacodda.nitid.image";"#, r"HKCU:\Software\Classes\lacodda.nitid.image"),
        (
            r#"key: r"Software\Classes\Directory\shell\nitid""#,
            r"HKCU:\Software\Classes\Directory\shell\nitid\command",
        ),
        (r#"const RUN_VALUE: &str = "nitid";"#, r#""*nitidw.exe*--resident""#),
    ];
    for name in ["release.yml", "install.yml"] {
        let workflow = read(&format!(".github/workflows/{name}"));
        for (source, asked) in written {
            assert!(program.contains(source), "src/install.rs no longer has `{source}`");
            assert!(workflow.contains(asked), "{name} does not look for `{asked}`");
        }
    }
}

/// And for the thumbnails: the DLL by its name, the class by its number, and
/// Windows itself by the script that asks it - on the machine a release is
/// rehearsed on and, before that, on every push.
#[test]
fn the_rehearsal_asks_windows_for_a_thumbnail() {
    for name in ["release.yml", "ci.yml"] {
        let workflow = read(&format!(".github/workflows/{name}"));
        assert!(workflow.contains("tools/check-thumbnails.ps1"), "{name} asks Windows for no thumbnail");
        assert!(workflow.contains("nitid_thumbs.dll"), "{name} does not look for the DLL");
    }
    let script = read("tools/check-thumbnails.ps1");
    for sample in ["JXL", "AVIF", "HEIC"] {
        assert!(script.contains(&format!(r#"(Get-Sample "{sample}")"#)), "the script asks for no {sample}");
        assert!(
            read("src/thumbs/samples.rs").contains(&format!("const {sample}: &str = concat!(")),
            "the script reads a sample called {sample} and there is none"
        );
    }
}

/// `rav1d` does not compile without NASM, and says so in a way that reads like
/// something else. Every job that compiles has it - a release job that has not
/// fails after the tag.
#[test]
fn every_job_that_compiles_has_nasm() {
    for (name, workflow) in workflows() {
        let mut jobs: Vec<&str> = workflow.split("\njobs:\n").nth(1).unwrap_or_default().split("\n  ").collect();
        // Split at two spaces also splits at four; a job starts where the
        // third character is not a space.
        let mut whole: Vec<String> = Vec::new();
        for part in jobs.drain(..) {
            if part.starts_with(' ') || part.starts_with('#') || whole.is_empty() {
                match whole.last_mut() {
                    Some(job) => job.push_str(&format!("\n  {part}")),
                    None => whole.push(part.to_string()),
                }
            } else {
                whole.push(part.to_string());
            }
        }
        for job in whole {
            let compiles = job
                .lines()
                .map(str::trim)
                .filter_map(|line| line.strip_prefix("run: ").or(Some(line)))
                .any(|line| {
                    ["cargo build", "cargo check", "cargo test", "cargo clippy", "lyrn check"]
                        .iter()
                        .any(|command| line.starts_with(command))
                });
            assert!(
                !compiles || job.contains("ilammy/setup-nasm@"),
                "a job of {name} compiles nitid without NASM: {}",
                job.lines().next().unwrap_or_default()
            );
        }
    }
}

/// The notes end with the three boxes a release is taken through, the first of
/// which the release engine ticks by its words.
#[test]
fn the_notes_end_with_what_was_checked() {
    let notes = read(".github/release-notes.md");
    let boxes: Vec<&str> = notes.lines().filter(|line| line.starts_with("- [ ] ")).collect();
    assert_eq!(boxes.len(), 3, "{notes}");
    assert!(boxes[0].contains("tools/install.ps1") && boxes[0].contains("SHA-256"), "{}", boxes[0]);
    assert!(read(".github/workflows/release.yml").contains("cat .github/release-notes.md >> RELEASE_NOTES.md"));
}

/// Each architecture decision has a number of its own, in one unbroken run: a
/// release's notes and the pages point at them by it.
#[test]
fn the_decisions_are_numbered_without_a_gap() {
    let numbers: Vec<u32> = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/adr"))
        .expect("the decisions")
        .map(|entry| entry.expect("a decision").file_name().to_string_lossy().into_owned())
        .map(|name| {
            name.get(..4)
                .and_then(|digits| digits.parse().ok())
                .unwrap_or_else(|| panic!("{name} has no number"))
        })
        .collect();
    let unique: BTreeSet<u32> = numbers.iter().copied().collect();
    assert_eq!(unique.len(), numbers.len(), "two decisions share a number");
    assert_eq!(unique.iter().copied().collect::<Vec<_>>(), (1..=numbers.len() as u32).collect::<Vec<_>>());
}
