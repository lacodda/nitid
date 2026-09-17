---
title: One window
description: Why a second image goes to the viewer already running.
---

Double-clicking a second image does not open a second viewer. The launch finds
the one already running, hands its file over and exits, and the picture appears
in the time it takes to decode — measured at 135 to 155 milliseconds against
320 to 560 for a cold start, because the window and the graphics device are
already there.

The same answers multi-select. Windows starts one process per selected file, so
five files means five launches; four of them hand their file to the first, and
the five arrive as one list in one window. Arrow keys then walk that selection
rather than the whole folder — the five you picked, not the hundreds beside
them. Five files opened this way took 239 milliseconds altogether.

The window also comes forward when it takes a file. Windows only lets the
process the user is working in raise a window, and after a double-click that
process is the launch, not the viewer already running — so the launch hands
that right over as it connects, naming the window it found. Without it a
default viewer would change its picture behind whatever you were looking at.

The window that owns the channel is simply the first one to create it, which is
a single atomic call, so two launches racing cannot both decide they are the
window. Nothing polls: a hand-over wakes the event loop the same way a finished
decode does, and a still image still costs no wakeups at all. See
[ADR 0016](https://github.com/lacodda/nitid/blob/main/docs/adr/0016-one-window-elected-by-a-named-pipe.md).
