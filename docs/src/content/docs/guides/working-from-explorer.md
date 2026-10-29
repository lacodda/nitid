---
title: Working from Explorer
description: Space on a picture in Explorer puts it on screen and Space again puts it away; "Open with nitid" on a folder walks its pictures.
---

Most pictures are found in Explorer before they are looked at, so nitid meets
them there: a glance with the space bar, and a folder opened from its menu.
Both come with `nitid install`.

## A look with the space bar

Select a picture in Explorer or on the desktop and press **Space**: it is on
screen. Press **Space** again and it is gone, and you are back in Explorer
where you were. In between it is the viewer as ever - the arrow keys walk the
folder, the wheel zooms, `I` says what the file is.

- **Several pictures selected** are walked on their own, the way a
  double-click on them opens them.
- **One folder selected** is walked from its first picture.
- **A viewer that was already open** takes the look: the picture comes up in
  it, and **Space** sends the window back behind Explorer, keeping what it now
  shows, rather than closing it. Opened from Explorer any other way - a
  double-click, *Open with* - it is an ordinary window again, and **Space**
  steps to the next picture.
- During a slideshow or a comparison **Space** is theirs, as it is anywhere
  else; `Esc` still closes the window.

The space bar stays Explorer's when it is not a look: in a name being typed -
renaming a file, the address bar, the search box - in the tree of folders on
the left, with `Ctrl` or `Shift` held, and while you are typing a file's name
to find it in the list, which Explorer does as you type. A space within a
second of a typed letter is part of the name.

The first look starts the viewer, which takes about a third of a second; with
a window already open, a look is a hand-over and the picture is there in the
time it takes to decode.

## nitid in the background

The space bar is heard by a small part of nitid that keeps running when no
picture is open. It holds no picture and no graphics device, and it shows
itself as nitid's mark in the notification area, beside the clock - in the
overflow (`^`) unless you drag it out. Its menu has **Open nitid**, which is
the desktop shortcut, and **Quit**.

It starts when you sign in. To stop that, switch nitid off in **Settings →
Apps → Startup**, where Windows lists everything that starts with you. After
**Quit** it is back at the next sign-in, or at once with `nitid install`.
`nitid uninstall` stops it and removes the entry.

Only Explorer and the desktop are heard. Other file managers, and the *Open*
and *Save* dialogs of other programs, are lists of files Explorer does not
answer for.

## Open with nitid on a folder

Right-click a folder and choose **Open with nitid**: the viewer walks its
pictures, starting where its order starts - the first by name, or the newest
or the largest if you sort that way in the settings. Right-clicking the empty
space inside an open folder does the same for that folder. A folder with no
pictures in it says so, and opens the folders you have been to instead.

On Windows 11 the entry is under **Show more options** (or `Shift+F10`): the
top of that menu is kept for programs installed as signed packages.

The same works from a terminal, and handed to a window that is already open,
a folder takes it over:

```
nitid D:\Photos\2026-10-28
```
