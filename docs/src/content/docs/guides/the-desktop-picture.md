---
title: The desktop's picture
description: W puts a picture on the desktop; Shift+W or nitid wallpaper changes it from a folder on a schedule, new pictures first, with nothing left running.
---

**`W` puts the picture on screen on the desktop.** It stays there: if a folder
was changing the desktop on a schedule, `W` stops that and says so, because a
person who chose one picture wants that picture to stay.

**`Shift+W` changes the desktop's picture from the open folder on a schedule**,
starting with the picture on screen — every 30 minutes unless the settings say
otherwise. `Shift+W` again, on the same folder with the same pattern, stops it.

## A folder, dealt like a shuffled slideshow

Each change deals from the folder the way `Shift+S` does: every picture once a
round, in a random order, and never the same picture twice in a row across the
seam between rounds. One thing comes before the shuffle: **a picture that
arrived in the folder since the last change goes up next**, newest first. That
is what makes the folder the source rather than a snapshot of it — drop a
picture in, and it is the next one on the desktop.

**The name pattern goes with it.** With `*.png` up on `Ctrl+F`, `Shift+W`
changes the desktop from the folder's PNGs only, and says so: "the desktop
changes every 30 min from Walls (*.png)". Marks (`M`) do not travel — the
schedule reads names, not the files' contents.

## What the desktop is given

Not the file itself, but a picture made for the desktop, because the desktop is
not the viewer:

- **In sRGB.** Windows does not manage the desktop's colour. A wide-gamut or
  HDR picture is converted the way an export with the colour baked in is, so it
  looks on the desktop as it looked in nitid on an sRGB screen. An HDR picture
  becomes SDR as it would on an ordinary screen: highlights above white clip.
- **Upright.** A photograph taken with the phone on its side carries its turn
  as a tag, which the desktop ignores; the picture it gets is turned already.
- **In a format Windows reads.** HEIC, AVIF, JPEG XL, WebP, SVG — anything nitid
  opens — goes up as a JPEG. An animation goes up as its first frame.
- **No bigger than the largest display needs**, in real pixels, so a
  sixty-megapixel photograph is not handed whole to the desktop to shrink. A
  photograph is never made bigger than it is; a vector is drawn at whatever
  size the display needs, sharp.

It goes on every display, filling each screen — cut where the shapes differ —
or, set to *whole on the screen*, shown whole with the desktop's colour round
it.

The Desktop section of the settings (`,`) says what is changing the picture and
has a button to stop it, and sets how often a folder changes it — from five
minutes to a day — and whether it fills the screen or shows whole. The folder
itself is chosen by opening it and pressing `Shift+W`: a folder is something
you look at before you pick it, not a path typed into a box.

## Nothing stays running

The schedule is a task in the Windows Task Scheduler, called **nitid wallpaper**,
that starts `nitidw.exe wallpaper next` and is gone again in a second or two —
nothing of nitid waits in the background between changes, and nothing else has
to be installed. The task runs on battery as well as on the mains, a change
missed while the machine slept happens when it wakes, and it runs only while you
are signed in, because the desktop is yours. You can see it, run it or delete it
in the Task Scheduler like any other.

`nitid uninstall` removes the task, so it never goes on starting a program that
is no longer there. See
[ADR 0035](https://github.com/lacodda/nitid/blob/main/docs/adr/0035-the-desktop-picture-changes-by-a-scheduled-task.md)
for why it is a task rather than a program left running or Windows' own
slideshow.

## From a terminal

```
nitid wallpaper D:\Pictures\Walls
nitid wallpaper D:\Pictures\Walls --every 2h --only "*.jpg;*.png" --fit fit
nitid wallpaper --every 1d
nitid wallpaper
nitid wallpaper next
nitid wallpaper off
```

The first starts changing the desktop from the folder and puts the newest
picture up now. Options without a folder change how the running schedule works.
On its own, `nitid wallpaper` says what is going on: the folder, how often,
when the next change is, and why the last one failed if it did — a folder
emptied or taken away, say. `next` changes it now, `off` stops it.

`--every` takes `5m`, `90m`, `2h`, `1d` or a bare number of minutes, from a
minute to 31 days. `--only ""` takes the pattern off.

## Where it is kept

| What | Where |
| --- | --- |
| The folder, the pattern, how often, how it fits | `%APPDATA%\lacodda\nitid\wallpaper.conf` |
| What has been on the desktop, and the pictures made for it | `%LOCALAPPDATA%\lacodda\nitid\wallpaper\` |

`wallpaper.conf` is plain lines, like the viewer's own settings, and an interval
changed in it by hand reaches the task on its next run. It is a file of its own
rather than part of `settings.conf` because an open viewer writes `settings.conf`
whole when it closes, and would undo a change made meanwhile from a terminal.

Two pictures are kept for the desktop and used in turn, so the one on the
desktop is never the one being written: a change that fails half-way leaves the
desktop as it was.

If the desktop shows Windows' own *Spotlight* pictures, putting a picture up
turns Spotlight off, as choosing a picture in the Settings app does. Choosing
*Windows spotlight* under *Personalization → Background* brings it back.
