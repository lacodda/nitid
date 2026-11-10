# 45. The Jump List is the start screen's list

Date: 2026-11-10

## Status

Accepted.

## Context

Since v0.39 the viewer remembers where it has been: a folder to an entry,
newest first, each holding the picture last looked at there (ADR 0037). The
start screen draws that list when nitid starts with nothing to show. The
taskbar offered nothing: a right click on nitid's button showed only "nitid",
"Pin" and "Close", so going back to this morning's screenshots meant starting
the viewer first and choosing from inside it.

Windows has a place for exactly this, a Jump List, and three ways to fill it:

- **`SHAddToRecentDocs`**, letting the shell keep a recent list of files. The
  shell's list is of files the viewer opened, not of folders it was in, and
  holds what Explorer thinks, not what nitid's own list holds; two lists of
  the same thing drift apart.
- **The known Recent category** — the same list, drawn by the shell.
- **A custom category** written with `ICustomDestinationList`, from nitid's
  own list.

A Jump List also belongs to an application id, and nitid had none of its own.
Windows makes one up from the executable's path, and nitid is two executables
(ADR 0004): the same viewer was two taskbar buttons depending on whether a
terminal or Explorer opened it, and a Jump List written for one would be
missing from the other.

## Decision

**The Jump List is a custom category, "Recent", written from `recent`.** One
line to a folder, newest first, as many as the shell gives room for, each a
shortcut to `nitidw.exe` with the picture's path — the same entry a tile is,
opening on the same picture. Folders that are not there are left off, as the
start screen leaves them off.

**One list, both ways.** A folder forgotten on the start screen is gone from
the Jump List at the next write. A line a person removes from the Jump List
comes back from the shell the next time the list is written, and its folder
is forgotten in `recent` — the shell refuses a list that puts a removed line
back, and a person who removed a folder in one place meant it in both.

**A name of its own: `lacodda.nitid`.** Every viewer process claims it before
its first window, and the desktop shortcut carries it — new ones when they are
made, ones made before v0.43 when nitid is next installed or updated, and only
if they point at the installed viewer. The name never changes: pins,
shortcuts and the list are all filed under it.

**Written off the event loop.** The shell's list is COM and loads part of
Explorer into the process. A thread of its own writes it: once after the first
frame — never before it — then whenever a folder comes to the front, and as the
viewer closes, with the window already hidden and at most a second's wait.
The same list twice is not written again. Removals reach the viewer through a
box it empties on an event and once more as it closes, so the last write's
are not lost and none is acted on twice.

**Gates never reach the shell.** `NITID_JUMP_LIST_RECORD=<file>` writes the
lines to a file instead, and every gate that starts a viewer sets it
(`tests/common`). The shell's half has its own test, under an id of its own,
which reads the list back from where the shell keeps it and deletes it again.

## Consequences

- A minimised viewer reached from its Jump List has to come to the front, not
  only come back; that defect (the restore happened before the foreground was
  taken) is fixed in the same version.
- A pin made from a window that the console binary opened points at
  `nitid.exe`. The window does not yet say how it is relaunched
  (`System.AppUserModel.RelaunchCommand`), which would point every pin at
  `nitidw.exe`; nitid is opened from a terminal rarely enough for that to wait.
- With "Show recently opened items" off, the shell refuses the category; the
  list is committed empty, which is the setting's meaning. The start screen is
  nitid's own and keeps its folders.
