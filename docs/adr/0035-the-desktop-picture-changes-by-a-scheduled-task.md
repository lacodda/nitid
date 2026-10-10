# 35. The desktop's picture changes by a scheduled task, from a picture made for it

Date: 2026-10-10

## Status

Accepted.

## Context

v0.37 lets nitid change the desktop's picture from a folder on a schedule. Three
things have to be decided: what keeps time between changes, what Windows is
given, and where the settings live.

Windows can already rotate the desktop through a folder by itself
(`IDesktopWallpaper::SetSlideshow`). It reads only the formats Windows decodes
— not HEIC, AVIF or JPEG XL without store extensions, not SVG — knows nothing
of a name pattern, and does not put a picture that has just arrived first.

The desktop is not the viewer. Windows does not manage its colour, ignores a
photograph's orientation tag, and gets the scaled-down size of a scaled display
when the program asking has not said it knows about scaling.

## Decision

**A task in the Windows Task Scheduler keeps the time.** One task, `nitid
wallpaper`, in the root of the library, runs `nitidw.exe wallpaper next` every
interval: the windowed binary, so no console window flashes up on the desktop.
Nothing of nitid stays running between changes. The task is registered whole
from the Task Scheduler's own XML through `ITaskService`, because the settings
that matter are ones `schtasks` cannot set: start on battery and keep going on
battery (a default task does neither, and on a laptop would change the desktop
only at a desk), run a change missed during sleep when the machine wakes, never
start a second run over a first, run only while the person is signed in, and
give up after five minutes. The first run is one interval after the picture
changed on the command; the start is written in UTC, so no time-zone table is
needed.

Rejected: a nitid process left running to keep time — a process held for the
sake of one change every half hour, and one more thing to start with Windows;
and Windows' own slideshow — for the formats, the pattern and the arrivals
above.

**Windows is given a picture made for the desktop.** Decoded by nitid's own
path (the sandbox for the formats that need it), converted to sRGB at the depth
it was decoded at — as an export with the colour baked in is — made no bigger
than the largest display needs in physical pixels (asked with the thread's DPI
awareness set to per-monitor), turned upright, and written as a JPEG at quality
95. A vector is drawn at the size needed instead. Two such files are kept and
used in turn, so the one on the desktop is never the one being written. It goes
on every display through `IDesktopWallpaper::SetWallpaper` with no monitor
named, the call the Settings app uses, so it survives a restart.

**The deal is a deck, with arrivals first.** Every picture once a round, a new
round never opening on the picture that closed the last — the slideshow's deck
— except that a name not in the folder at the last change goes first, newest
first. The deck is written to a file after each change: the next change is
another process.

**The settings live in a file of their own,** `%APPDATA%\lacodda\nitid\
wallpaper.conf`, written the moment they change; the deck and the pictures in
`%LOCALAPPDATA%\lacodda\nitid\wallpaper\`. Not in `settings.conf`: an open
viewer writes that file whole when it closes, and would silently undo a change
made meanwhile with `nitid wallpaper`. The settings file is the one truth about
the interval: a run that finds it differs from the interval the task was made
with makes the task again.

## Consequences

- `W` (this picture, and the schedule off) and `Shift+W` (this folder, from the
  picture on screen, with the walk's name pattern; again to stop) run on a
  worker thread: preparing a large photograph and talking to the Task Scheduler
  both take long enough to stall a key.
- Putting a picture up turns off Windows Spotlight on the desktop, as choosing a
  picture in the Settings app does.
- `nitid uninstall` removes the task; a task left behind would start a program
  that is not there, every interval, for ever.
- No test touches the real desktop or the Task Scheduler. The picture made for
  the desktop is held to account from real files of each kind
  (`tests/live_wallpaper.rs`); the task's XML by a unit test of every setting
  above. `NITID_DESKTOP_RECORD=<file>` writes what the desktop would have been
  told into a file, so the whole way there — folder, deck, picture, schedule,
  the scheduled run itself — can be run on a desktop that is not the test's to
  change.
