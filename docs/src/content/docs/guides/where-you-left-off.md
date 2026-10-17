---
title: Where you left off
description: Started from its icon, nitid opens on the folders it has been to; O brings them up over a picture, Ctrl+O opens any other, and a hotkey shows the newest picture from anywhere in Windows.
---

Most of the time nitid is opened on a file: you double-click a photograph and
it is there. Started from its icon, the Start menu or a pinned taskbar button,
it has no file to show — so it shows where you have been.

## The start screen

**One tile to a folder, newest first.** Each tile is the picture you were
looking at when you last left that folder, with the folder's name, the
picture's name and how long ago it was. Click a tile — or move to it with the
arrow keys and press `Enter` — and nitid carries on from that picture, with
the rest of its folder beside it for the arrow keys. A picture deleted since
is not a dead end: the folder opens on its first picture instead.

The tiles fill in the way a picture does: each file's embedded thumbnail
first, in milliseconds, and then the picture itself, shrunk to the tile, in
the display's colours — the same colours the picture will have when it
opens. Nothing is cached on disk; the pictures are made again the next time
nitid starts.

A folder on a drive that is not plugged in is left off the screen but not
forgotten: put the drive back and its tile returns. To take a folder off the
list for good, point at its tile and click the cross in the corner, or press
`Del` on it.

## Back to it from a picture

`O` brings the start screen up over the picture you are looking at — or click
the mark and the name at the left of the toolbar. The keyboard starts on the
second tile, the folder before this one, the way switching windows starts on
the window before this one: the first tile is where you already are. `O` or
`Esc` goes back to the picture.

While the screen is up nothing changes the picture behind it: a slideshow
waits, and a followed folder holds its newest picture until you come back.

## Any other picture

`Ctrl+O`, or **Open a picture…** on the start screen, opens the Windows open
dialog in the folder of the picture on screen. Choose one picture and nitid
walks its folder; choose several and it walks just those, the way a selection
handed over from Explorer is walked. A picture can also be dropped on the
window, start screen or not.

## The newest picture, from anywhere

**`Win+Alt+N`, pressed in any program while nitid is open, brings nitid
forward with the newest picture in the folders on the start screen** — the
one most lately written, not the one most lately looked at. Take a screenshot
with `Win+Shift+S`, let a generator write its next frame, import a card from
the camera: press the key and you are looking at it.

It answers while nitid is open, minimised or behind other windows. A closed
viewer is a process that has ended, and there is nothing of it left to
listen. The key can be changed, or turned `off`, under **Settings → Opening →
Hotkey**; type the combination — `Ctrl+Alt+F13`, `Win+Shift+P` — and it
takes effect at once. A combination another program already holds is said
so, in a message and under the box, rather than left to do nothing: choose
another. A key with no `Win`, `Ctrl` or `Alt` is refused unless nobody types
with it — F13 to F24, `Pause`, `Print Screen` — because a letter taken from the
whole desktop is a letter gone from every program.

## Where the list lives

`%LOCALAPPDATA%\lacodda\nitid\recent`: one line to a folder, the moment and the
path with a tab between them, newest first, up to 24 folders. Local rather
than roaming, because the paths are this machine's. A picture pasted from the
clipboard is never written there — it has no file. The file is joined with
what is already on disk when it is written, so two windows opened with
`NITID_NO_SINGLE_INSTANCE` do not wipe out each other's folders. See
[ADR 0037](https://github.com/lacodda/nitid/blob/main/docs/adr/0037-the-start-screen-is-a-folder-to-a-tile.md)
and [ADR 0038](https://github.com/lacodda/nitid/blob/main/docs/adr/0038-a-global-key-is-held-by-a-thread-of-its-own.md).
