---
title: Printing
description: Ctrl+P prints the picture on screen through the Windows print dialog, whole on the page or filling it, in the colour you see.
---

**`Ctrl+P` opens the Windows print dialog** — the system's own, the one the
Photos app opens, in the language Windows speaks, with the printers,
paper sizes and options your driver offers. The preview on its right is the
page as it will come out of the printer.

The page starts turned the way the picture is: a picture wider than it is tall
starts on a landscape sheet, so it does not use half the paper. Change the
orientation, the paper or the printer in the dialog and the preview follows.

## Whole on the page, or filling it

One option in the dialog is nitid's own: **Picture**.

- **Whole on the page** — the default. The whole picture, as large as the
  printable area allows, centred, with white paper round it where the shapes
  differ.
- **Filling the page** — the picture covers everything the printer can reach,
  and what overhangs is cut off evenly at both sides. Print without borders as
  well, on a printer that can, and that is the whole sheet.

The choice is kept while nitid runs, so a stack of pictures is printed the same
way without choosing again. The next time nitid starts, a page starts whole: it
is the one choice that never cuts anything off.

## What is printed

The picture as it is on screen, not the file as it is stored:

- **In the colour you see.** The picture is converted to sRGB the way an export
  with the colour baked in is — the shader's own path — and handed to the
  printer as sRGB, the one colour space every printer driver is certain to read
  right. The driver maps it to its inks the way it maps everything else it
  prints. An HDR picture is printed as it shows on an ordinary screen:
  highlights above white clip, as they do in an export.
- **Turned the way it is shown** — the photograph's own orientation and any
  turn you made with `R`, saved or not.
- **The frame that is up**, for an animation; **drawn for the paper**, for an
  SVG, so a ten-pixel icon printed across a page is sharp, not enlarged.
- **Transparency on white**, because that is what paper is.

The printer is sent the part of the picture that shows, at **300 pixels to the
inch** of the paper it covers — the resolution photographic printing is made
for. A sixty-megapixel photograph printed postcard-size is not sent whole; a
small picture printed across a page is sent with the pixels it has and enlarged
by the printer, as any print of it would be.

A picture pasted from the clipboard (`Ctrl+V`) prints like any other.
