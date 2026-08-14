#!/usr/bin/env python3
"""Add the unreleased section to CHANGELOG.md, leaving the rest alone.

`git-cliff -o` rewrites the whole file, which erased a hand-written Breaking
Changes note twice (ADR 0006). `--prepend` leaves history alone but writes to
the very top: with the header it duplicates the title block, and with
`--strip header` the new section lands *above* the title. Neither is right on
its own, so this generates the section and inserts it after the header.

Usage: python tools/changelog.py v0.6.0
"""

import subprocess
import sys
from pathlib import Path

CHANGELOG = Path(__file__).resolve().parent.parent / "CHANGELOG.md"


def main() -> int:
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} vX.Y.Z", file=sys.stderr)
        return 2

    tag = sys.argv[1]
    section = subprocess.run(
        ["git-cliff", "--tag", tag, "--unreleased", "--strip", "header"],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=True,
    ).stdout.strip()

    if not section:
        print("nothing unreleased to write", file=sys.stderr)
        return 1

    text = CHANGELOG.read_text(encoding="utf-8")
    # The header is everything before the first release heading.
    first_release = text.index("## [")
    CHANGELOG.write_text(
        text[:first_release] + section + "\n\n" + text[first_release:],
        encoding="utf-8",
    )

    print(f"{tag} written to {CHANGELOG.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
