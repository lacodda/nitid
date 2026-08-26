# 16. One window, elected by a named pipe

Date: 2026-08-26

## Status

Accepted.

## Context

Opening an image from the shell starts a process, and that process pays for a
window and a graphics device before it can draw anything. Measured on the
development machine, a 1200×800 JPEG:

```
window created at    46 ms
gpu ready at        236 ms
first pixels in     318 ms
```

Three runs gave 318, 332 and 556 ms, of which 190 to 340 ms sits between
"window created" and "gpu ready" — the graphics device coming up. A viewer that
is already open has paid all of that.

Multi-select makes the same cost worse in a way that is also visibly wrong. The
registry entry the shell reads is `"nitidw.exe" "%1"` — one file per launch — so
selecting five images and pressing Enter starts five processes and opens five
windows. Measured: three files, three processes, three windows.

## Decision

The first launch to create a named pipe owns the window. Any launch that finds
the pipe already there sends its paths down it and exits.

**The election is `CreateNamedPipeW` with `nMaxInstances = 1`.** Creating a pipe
instance is atomic, so of two processes racing exactly one succeeds and the
other is told `ERROR_PIPE_BUSY`. That was measured rather than assumed, and so
was the alternative: with `nMaxInstances = 2` *any* process can create the
second instance, and the election is gone. No lock file to go stale, no mutex to
leak when a process is killed — the pipe disappears with the process holding it,
which was also measured.

**A messenger tells "busy" apart from "absent" and treats them differently.**
Measured: a pipe nobody owns answers `ERROR_FILE_NOT_FOUND` (2); a pipe whose
single instance is serving somebody else answers `ERROR_PIPE_BUSY` (231).
Absent means open a window; busy means a window is definitely there, so retry.
Giving up on busy would open a second window for no better reason than arriving
second in a multi-select burst. `WaitNamedPipeW` looks like the answer to busy
and is not — measured against a busy one-instance pipe it returns false at once,
because the instance is connected rather than pending.

**A hand-over wakes the event loop through the proxy a finished decode already
uses.** The listener runs on its own thread and sends a user event; nothing
polls. The promise from v0.12.0 — a still image costs no wakeups — is untouched.

**Several files browse themselves, not their folder.** One file opens its
folder as before; a selection of several becomes the list the arrow keys walk.
Picking five images and then being able to arrow through four hundred is not
what the selection meant.

**Files that arrive at an open window are added, not substituted.** The window
was showing something the user was looking at; the arriving file becomes the
current one and the rest of the list stays.

## Consequences

A second image opens in 135 to 155 ms instead of 320 to 560, and five selected
files open in one window in 239 ms instead of five windows.

Three defects were found by measurement while building this, each of which
would have been a viewer that stops accepting files:

- **A hang-up killed the listener.** Anything on the machine may connect to a
  pipe it finds and close without saying anything; that is reported as
  `ERROR_NO_DATA` on `ConnectNamedPipe`, and treating it as a failure ended the
  accept loop — after which the window never took another file. It is now one
  of two outcomes that are not failures at all.
- **A write with no reader blocks for ever.** A message larger than the pipe's
  buffer waits for somebody to drain it, and against a window that never reads,
  waits indefinitely. Messages are capped at half the buffer, so a write always
  completes; an oversize selection is refused before connecting, and the caller
  opens its own window.
- **A messenger could still hang on a wedged window.** The whole hand-over now
  runs on a thread the process gives up on after ten seconds. A messenger that
  never exits is worse than one that opens a second window.

`NITID_NO_SINGLE_INSTANCE` turns sharing off, and the startup gate sets it: that
gate measures a *cold* start, and a viewer left open on the machine running the
tests would otherwise take the file and have it report the time to write to a
pipe — a number with nothing to do with what a user waits for, in a gate that
could then never fail. `NITID_INSTANCE_ID` narrows the channel so a test never
talks to the viewer the developer has open.

The pipe name carries the user, because a pipe is machine-wide while a viewer is
not: two people signed into one machine each get their own window.

Not addressed: a hand-over does not carry the window to the foreground on
Windows' terms. `focus_window` is asked for, and Windows may refuse a process
that does not own the foreground — the file still arrives and the window still
updates. Doing better means `AllowSetForegroundWindow` in the messenger, which
is worth it only if the current behaviour proves annoying in daily use.
