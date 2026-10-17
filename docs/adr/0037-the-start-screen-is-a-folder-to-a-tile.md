# 37. The start screen is a folder to a tile, kept in a list of its own

Date: 2026-10-17

## Status

Accepted.

## Context

Until v0.39 a viewer started with no file — from its icon, the Start menu, a
pinned taskbar button — opened an empty window. Worse than empty: the interface
waits for the first picture before it lays anything out (the photograph has
the first frame), so with no picture it never laid out at all, and the window
had no chrome, no message, nothing to press. Every other way of starting nitid
names a file; this one was the only launch with nothing to show, and it showed
exactly that.

The line's plan already had a place for what it should show: one entry without
a file, grown over three majors — a first grid of recent places here, a filter
for screenshots and capture sessions on it in 2.x, a cached and virtualised
gallery in 3.0.

Three questions had to be answered before a grid could be drawn: what a tile
stands for, where the list is kept, and where the tiles' pictures come from.

## Decision

**A tile is a folder, holding the picture last looked at in it.** Not a list of
recent folders beside a list of recent files: a folder alone does not say
where to start, and a file alone is something nitid never opens anyway — one
file browses its folder. The entry is both. Opening a tile opens its picture in
its folder, so the arrow keys carry on from where they stopped; a picture
deleted since lands on the folder's first picture rather than on an error.

**The list is a file of its own, in `%LOCALAPPDATA%\lacodda\nitid\recent`.**
Local rather than roaming, because the paths are this machine's. Not in
`settings.conf`, which is written whole when the window closes and holds
choices, not history. Plain lines — the moment in seconds, a tab, the path —
newest first, at most 24, because a path holds spaces but never a tab or a line
break, and a person opening the file can read it.

**Written when a folder comes to the front, and when the window closes.** A
step to the next picture in the same folder only changes which picture the
entry holds; writing the file on every arrow key would be a disk write per
keystroke for a detail the closing writes anyway. The write reads the file
first and joins it, a folder both copies know keeping the later visit, so a
second window opened with `NITID_NO_SINGLE_INSTANCE` does not erase the first
one's folders; a folder taken off the list in this session stays off,
whatever the file on disk still says.

**A folder that is not there is hidden, not dropped.** A drive that is
unplugged today is plugged in tomorrow. It leaves the list the ordinary way,
pushed off the end by newer folders.

**The tiles' pictures are made on a thread of their own, in two passes.**
Every tile's embedded thumbnail first — milliseconds each — and then the full
decodes, through the same decoder and sandbox as the viewer, for the tiles the
first pass could not make sharp. Not through the loader: its cache is the
neighbourhood of the picture on screen, and a screen of tiles pushed through it
would evict the neighbours that make the arrow keys instant. Shrunk by
averaging a grid of samples per pixel rather than by sampling one — fine detail
sampled thirtyfold smaller turns to noise — and painted in display colours,
like the minimap, so a tile is the colour of the picture it opens. The pass
gives up between files as soon as the screen closes.

**No thumbnail cache on disk.** That is the gallery's foundation in 3.0, and a
half-built cache here would be a second one to migrate then.

**Opening from a picture starts on the second tile.** Over a picture the first
tile is where the person already is; the second is the folder before it, the
way switching windows starts on the window before this one.

## Consequences

A bare start lays out its first frame as the start screen, measured on the
debug build at the first frame after the GPU is ready; the pictures fill in
behind it. `tests/live_recent.rs` starts the real binary with a list in a
private data folder and holds both halves: a bare start draws one tile for each
folder that is there and none for the one that is not, and a picture shown is
in the list the next start reads.

The integration tests used to start the viewer with the developer's own
`APPDATA` and `LOCALAPPDATA`. They moved the developer's window when they
closed; with this list they would also have filled the start screen with
temporary folders that vanish, pushing the real ones off the end.
`tests/common::keep_data_in` now gives every launch in the suite a folder of
its own.

A picture pasted from the clipboard is never written down: it has no file.
