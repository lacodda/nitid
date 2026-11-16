---
title: Staying up to date
description: nitid asks GitHub once a day for a newer release and installs it in the background. What that sends, what it leaves alone, and how to switch it off.
---

Once a day nitid asks GitHub whether there is a newer release. When there is, it
downloads it and installs it in the background, then says so with a notification
from its icon. Click the notification to read what changed. There is nothing to
press and nothing to restart.

A window already open keeps the version it started as until you close it; the
next one is the new version.

## On a metered connection

On a connection Windows knows as metered - a phone's hotspot, a plan with a
data limit, roaming - nitid still asks, which takes a few hundred bytes, and
puts the download off. It fetches the release within an hour of the machine
being on a connection that is not metered. *Settings → Updates* says when a
release is waiting like this.

**Check for updates** on the icon's menu and `nitid update` do not wait: when
you ask, the release is fetched on whatever connection there is. Windows calls a
network metered when you mark it so in *Settings → Network & internet*, or when
the carrier's plan says it is. See
[ADR 0047](https://github.com/lacodda/nitid/blob/main/docs/adr/0047-a-metered-connection-is-asked-on-and-not-downloaded-on.md).

## What leaves the machine

One request a day, for one small file: the newest release's description. When
that names something newer, a second request fetches its zip - on a metered
connection, later. GitHub sees that
someone asked and from what address. The requests carry no version number and no
identifier, nothing says which pictures you look at, and no picture or file name
ever leaves the machine.

Only the part of nitid that runs in the background asks. The viewer itself
reaches no network, and neither does the process that decodes the heavy formats.

## Switching it off

**Settings → Updates → Keep nitid up to date.** Off, nitid asks nobody anything.
The same page says which version this is and how the last attempt went.

With it off, `nitid update` in a terminal still checks and installs once, when
you ask, and `nitid update --check` only says whether there is something newer.
So does **Check for updates** on the icon's menu.

If you have stopped nitid in the background with **Quit**, or switched it off in
*Settings → Apps → Startup*, nothing asks until it is running again.

## What an update leaves alone

An update replaces the program and nothing that is yours.

- **Your default programs.** If you chose nitid for a file type, it stays chosen.
  That choice belongs to Windows and an update does not touch it.
- **Anything you took away.** A desktop shortcut you deleted, the `PATH` entry
  you removed and the sign-in entry you took out all stay gone. Running the
  [install script](/nitid/getting-started/) again is what puts them back.
- **Your settings** and the folders on the start screen.

A file type that a new version learns to open joins the *Open with* list by
itself, without becoming anybody's default.

## When something goes wrong

What arrives is checked before anything runs: its size and digest against what
the release published, then each file's checksum, then the program itself is
asked which version it is. Once in place, it is asked again, from there. If any
of that fails, the copy that was there stays, or is put back, and it is still
what runs. Settings → Updates says what happened, and nitid tries again the next
day.

Both executables are replaced together or not at all, so a failure cannot leave
half of one version beside half of another.

## How it is built

Updates come from the same releases the install script uses, and a release is
offered to nobody until it is published. The releases are not code-signed, so what
you are trusting is the HTTPS connection to GitHub and the account that publishes
them - the same as when you first installed. See
[ADR 0043](https://github.com/lacodda/nitid/blob/main/docs/adr/0043-a-release-installs-itself-from-the-resident.md).
