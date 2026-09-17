---
title: Getting Started
description: Install nitid, make it the default viewer, and learn the handful of keys that matter.
---


Download the zip from the [latest release](https://github.com/lacodda/nitid/releases),
unpack it anywhere, and run:

```
nitid install
```

This copies nitid to `%LOCALAPPDATA%\Programs\nitid`, registers the file types
it can open, puts that directory on your `PATH` so `nitid` works as a command,
and leaves a shortcut on your desktop — no administrator, nothing outside your
own user account. Re-running it upgrades an existing install, even while the
viewer is open, and does not add the directory to the `PATH` a second time or
put a second shortcut beside the first.

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

`nitid uninstall` removes the files, the registration, the `PATH` entry, and
the desktop shortcut.

## The first minute

Double-click any image, or run `nitid picture.jpg`. Then:

- **Arrow keys** move through the folder.
- **Scroll** zooms; **space** with a drag pans.
- **`?`** shows every key there is.
- **`I`** says what the file says about itself.

The full list lives in [Keys and commands](/nitid/reference/keys/).
