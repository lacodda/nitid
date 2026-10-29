---
title: Getting Started
description: Install nitid, make it the default viewer, and learn the handful of keys that matter.
---


In PowerShell:

```
irm https://raw.githubusercontent.com/lacodda/nitid/main/tools/install.ps1 | iex
```

The script downloads the latest release and runs `nitid install` from it,
which copies nitid to `%LOCALAPPDATA%\Programs\nitid`, registers the file
types it can open and *Open with nitid* on folders, puts that directory on
your `PATH` so `nitid` works as a command, starts nitid in the background for
Explorer's space bar, and leaves a shortcut on your desktop - no
administrator, nothing outside your own user account. The script then checks
that the copy installed is byte for byte the one it downloaded. Running it
again upgrades, even while the viewer is open, and does not add the directory
to the `PATH` a second time or put a second shortcut beside the first.
Setting `$env:NITID_VERSION` to a release's tag first installs that release
instead of the latest.

A terminal that was already open keeps the environment it started with, so
`nitid` becomes available there once it is restarted.

The zip carries two executables and both are installed: `nitid.exe` is the one
to run from a terminal, and `nitidw.exe` is what the shell opens files with.
The second exists so that double-clicking an image never flashes a console
window — see [ADR 0004](https://github.com/lacodda/nitid/blob/main/docs/adr/0004-two-binaries-console-and-windowed.md).

Windows keeps the choice of default application to itself: no program is
allowed to seize a file type. After installing, nitid appears under **Open
with** — right-click an image, choose *Open with* → *Choose another app*, pick
nitid and tick *Always use this app*. It also shows up in *Settings → Apps →
Default apps*.

`nitid uninstall` stops nitid in the background and removes the files, the
registration, the folder menu, the sign-in entry, the `PATH` entry, the
desktop shortcut, and the desktop picture's schedule if one is running.

## The first minute

Double-click any image, or run `nitid picture.jpg`. Then:

- **Arrow keys** move through the folder.
- **Scroll** zooms; **space** with a drag pans.
- **`?`** shows every key there is.
- **`I`** says what the file says about itself.

And in Explorer, **Space** on a selected picture puts it on screen and
**Space** again puts it away - see [Working from Explorer](/nitid/guides/working-from-explorer/).

The full list lives in [Keys and commands](/nitid/reference/keys/).
