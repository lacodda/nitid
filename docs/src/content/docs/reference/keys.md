---
title: Keys and commands
description: Everything nitid answers to, in one table.
---

```
nitid photo.jpg
```

Opening a file opens its folder: the arrow keys walk the images beside it.

| Key | Action |
| --- | --- |
| `←` `→` | previous / next image in the folder |
| `Home` `End` | first / last image |
| `Space` | pause / resume an animation; next image on a still |
| Wheel | zoom around the cursor, or step through the folder |
| Ctrl+Wheel | whichever of the two the bare wheel is not |
| Drag | pan |
| Middle click | toggle fit and 100% |
| `+` `-` | zoom in / out |
| `0` `1` | fit to window / actual size |
| `Z` | hold for 100% under the cursor |
| `L` | hold the framing across a step |
| `R` | turn a quarter clockwise (`Shift+R` the other way) |
| `B` | what shows through transparency |
| `I` | what the file says about itself |
| `H` | what tones the picture is made of |
| `C` | mark what the file clipped |
| `P` | read the colour under the pointer, with the pixels around it magnified; click to copy |
| `K` | what is happening to this image's colour |
| `X` | frame a crop; Enter saves it as a copy, Esc leaves it |
| `Ctrl+Drag` | drag the picture into another window |
| `Ctrl+C` | copy the picture |
| `Ctrl+V` | show the picture on the clipboard |
| `Ctrl+Shift+C` | copy the path, quoted for a terminal |
| `Ctrl+Alt+C` | copy it as a JPEG small enough to send |
| `Del` | send this file to the recycle bin |
| `F2` | rename this file |
| `Ctrl+1`-`9` | move this file to the folder set for that key |
| `Ctrl+Shift+1`-`9` | copy it there instead |
| `F` | mirror it left to right; `Shift+F` top to bottom |
| `Ctrl+S` | write the turn into the file, without touching its pixels |
| `Ctrl+Shift+S` | save as another format, with the colour you see |
| `E` | open this file in the program that edits it |
| `Alt+1`-`9` | open it in the program set for that key |
| `F11` | fullscreen |
| `,` | settings |
| `?` | every key there is |
| `Esc` | close the settings, or quit |

"100%" means one image pixel per logical pixel, so a photo is the same size
here as everywhere else on a scaled display.

### Settings

`,` opens the settings, or the gear on the toolbar; `Esc` closes them. There is
no OK button — a change takes effect as it is made, so a threshold can be
dragged while watching what it marks. Four sections:

| Section | What it holds |
| --- | --- |
| Gestures | what the bare wheel does — zoom or step through the folder — how far one notch zooms, whether the wheel is reversed, whether the middle button toggles fit and 100% |
| View | when the toolbar and the status line are on screen: on hover, always, or never; when the minimap is — zoomed, always or never; and what shows behind transparency when a picture opens |
| Opening | fit or 100% for a picture that arrives, whether the framing is held across a step, whether the folder wraps at its ends, and the order it is walked in — name, date or size |
| Colour | where the clipping zebra draws its two lines, the units the eyedropper reads in, what a click copies, and whether the eyedropper magnifies the pixels around the pointer |
| Sending | the size budget and the maximum width `Ctrl+Alt+C` copies a picture to |
| Files | the nine folders `Ctrl+1`-`9` sort a picture into |

Ctrl+wheel always performs whichever gesture the bare wheel does not, so both
are reachable whichever way round the setting is.

Settings live in `%APPDATA%\lacodda\nitid\settings.conf`, one `key = value`
per line, meant to be readable and repairable by hand. A key the running
version does not recognise is left alone rather than dropped, so a newer
build's settings survive a run of an older one — see
[ADR 0022](https://github.com/lacodda/nitid/blob/main/docs/adr/0022-settings-are-plain-lines-that-survive-both-directions.md).

### Environment

| Variable | Effect |
| --- | --- |
| `NITID_STARTUP_REPORT=1` | print the startup breakdown to stderr, and state the surface each time it is configured |
| `NITID_EXIT_AFTER_FIRST_FRAME=1` | close as soon as a picture is on screen; used by the startup test |
| `NITID_TILE_LIMIT=<pixels>` | lower the texture side an image is cut into tiles at, so the tiled path can be exercised on a small file; never raises it past what the device accepts |
| `NITID_NO_SINGLE_INSTANCE=1` | open a window of this launch's own instead of handing the file to one already open; used by the startup gate, which measures a cold start |
| `NITID_INSTANCE_ID=<text>` | share a window only with launches carrying the same value, so a test never talks to the viewer you have open |
| `NITID_HANDOVER_REPORT=1` | print which process the foreground was offered to when a file is handed over; used by the one-window gate |
