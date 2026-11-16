# 48. A release is rehearsed by its workflow, and then published

Date: 2026-11-16

## Status

Accepted. Replaces the hand-published draft of ADR 0043's consequences.

## Context

Since v0.42 every installed copy fetches a published release within a day, and
a copy out there cannot be corrected (ADR 0043). So a release was drafted by
the workflow, downloaded and installed by hand on the development machine, and
published by hand once it had opened real pictures there. Only after that did
`install.yml` install it on a clean machine and update to it from the release
before - the one check that can see a break between versions ran when the
release was already being offered.

The rest of the line is released by one command. `furca release run` bumps,
writes the changelog, waits for CI, tags, waits for what the tag starts, checks
the release page against `release.toml`, installs the release here with
`tools/install.ps1` and proves the installed program is the archive's own. It
stops at a page that is still a draft, so nitid's hand-published draft kept it
out of that, and kept the release a ritual of a dozen steps somebody had to
remember in order.

## Decision

**The workflow that drafts the release rehearses it and publishes it.** Between
the draft and the publish, two clean Windows machines take the draft's own
files:

- one installs from nothing - the zip unpacked, `nitid install` run from it -
  and holds both programs to the archive's bytes, the version to the tag, and
  the registration, the folder menu and the sign-in entry to being there;
- one installs the release before with the README's script and has **that
  release's own updater** install the draft over it.

A release that fails either stays a draft, offered to nobody.

**The updater is rehearsed against this machine standing in for GitHub.** A
draft is not served at the address the updater reads, which is the point of
drafting. `NITID_UPDATE_FEED` already accepts `http://127.0.0.1:` and nothing
else, for the tests; the job lays the draft's `update.txt` and zip out as the
releases are laid out and serves them from there. What is rehearsed is the
whole of the old copy's path - manifest, download, digest, unpacking, the new
version installing itself - and what is not is GitHub's redirect, which the
same run through the real address covers a minute after the publish.

**`install.yml` is called by the release, not started by it.** A release
published by a workflow's own token starts no other workflow, so the run
through the real address is a job of the release workflow now, calling
`install.yml`. It still runs by itself when the script changes.

**`release.toml` describes the release to the engine.** One manifest
(`Cargo.toml`), a gate of `cargo fmt --check` and nothing that compiles - CI is
the judge, and the run waits for it - the workflow the tag starts, the two
files the page has to carry, and `[post.install]`. `tools/changelog.py` is
gone: the engine's `changelog` step does what it did, a section put under the
header with everything already in the file left byte for byte (ADR 0006).

**The installer hears what every installer of the line hears.**
`NITID_VERSION` as before; `NITID_NO_PATH`, read by `nitid install` itself
before it would touch the `PATH`; and `NITID_INSTALL_DIR`, which makes the
script put the two programs in that directory and do nothing else. The last is
not an install and does not pretend to be: `nitid install` has one place, which
the registration, the sign-in entry and `nitid uninstall` all name. It is what
lets `furca release verify --install` and `lyrn check --release --install`
prove a release's build without touching the machine they run on.

**The notes end with what was checked live.** `.github/release-notes.md` is
appended to the changelog's section. The engine ticks the first box when its
install proof passes; the rest are a person's, and `lyrn check --release` says
which are open.

## Consequences

Nobody opens a picture with a release before it is published. What a clean
machine in CI can hold - that it installs, that it is the build, that the
release before can reach it - is held before; what needs a real screen and a
GPU is checked on the development machine minutes after, from the published
release, and is the second box of the list. A release that turns out wrong on
a real screen is withdrawn by publishing the next one: an installed copy never
goes backwards, so withdrawing the page alone would leave it where it is.

The startup gate, the decoders and the colour path are CI's on every commit, as
before; the rehearsal adds the one thing they cannot see.

A tag whose rehearsal fails is out and stays. The fix is a new commit and the
next patch number, or the workflow run again for the same tag when the fault
was the runner's.
