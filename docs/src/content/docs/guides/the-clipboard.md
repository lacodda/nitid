---
title: The clipboard and drag and drop
description: Getting a picture in and out of the viewer.
---

`Ctrl+C` puts the picture on the clipboard, `Ctrl+V` shows whatever picture is
on it, and `Ctrl+Shift+C` copies the file's path — quoted, so a path with a
space in it survives being pasted into a terminal.

What is copied is **the file's own pixels**, unconverted, which is the same
decision the histogram and the eyedropper follow: the numbers on the clipboard
are the numbers those tools report. A `CF_DIB` has nowhere to say what its
numbers mean, so a wide-gamut picture will look flatter in an application that
assumes sRGB. That is a true thing about the Windows clipboard rather than
something a viewer should paper over by quietly rewriting the pixels on the way
out. Transparency is composited onto white, because `CF_DIB` carries no
dependable alpha and a cut-out is nearly always going onto a white page.

**`Ctrl+Alt+C` copies it as something sendable.** Two things go on the
clipboard at once, the way a drag offers two: a JPEG file, and the pixels. A
chat window or a mail client takes the file; an editor that paints takes the
pixels, exactly as it does today.

The file is made to a **budget in kilobytes**, not to a quality number. Nobody
knows what quality 74 weighs — it depends entirely on the picture — and
everybody knows what an attachment limit is, so the viewer searches for the
best-looking JPEG that fits and says which quality it settled on. The picture
is shrunk to a maximum width first, and its colour is baked into sRGB, because
what is travelling is a file going to a program that will very likely ignore a
profile. Both numbers are in the settings, at 500 KB and 2048 pixels to start
with. A budget nothing can meet is said so rather than quietly broken.

This is the one place the viewer writes a file without being asked for a file,
and it is asked for this one: the JPEG goes to a folder of its own inside the
temporary directory, named after the picture so what lands in a chat carries a
name that means something. Nothing appears in the folder you are looking at.

A pasted picture is **shown, not saved**. It has no file behind it: the title
says `clipboard`, the arrow keys have nowhere to go, and nothing is written to
disk. A viewer that quietly saved a temporary file on every paste would be
writing without being asked and leaving the results behind. See
[ADR 0020](https://github.com/lacodda/nitid/blob/main/docs/adr/0020-a-pasted-picture-is-shown-not-saved.md).

## Drag and drop


Files dropped on the window open. A selection of several arrives as the several
it was, and the arrow keys then walk those files rather than the hundreds
sitting beside them in the folder — the same thing multi-select does from the
shell. While files are held over the window it says so, because a drag with no
answer looks like a window that will not take it.

**`Ctrl` and a drag hands the picture the other way**, into a chat, a mail, an
editor. What travels is the file where there is one and the picture where there
is not, offered together so the receiving application takes whichever it
understands: a mail client gets the original file, with its format and its
metadata intact, and an editor that paints gets the pixels. Only a copy is ever
offered — a drag that could move the file would delete the photograph you are
looking at. See
[ADR 0021](https://github.com/lacodda/nitid/blob/main/docs/adr/0021-a-drag-offers-the-file-and-the-picture.md).

The bare drag stays panning. A picture pasted from the clipboard has no file to
hand over, so it travels as pixels only; nothing is written to disk to make it
look otherwise.
