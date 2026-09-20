---
title: Reading the picture
description: The histogram, the loupe and the colour tools that say what a pixel actually is.
---

Two things that answer questions the picture on screen cannot.

**The histogram**, with `H`: what tones the picture is actually made of, in the
corner rather than across the frame. The three channels are drawn in their own
colours and add where they overlap, so a colour cast shows as the curves
pulling apart; luminance goes over the top as a line, because that is the curve
an exposure is judged by. All four share one scale — drawn against their own
maxima a flat channel and a peaked one would look alike.

It counts **the values in the file**, before the colour transform. A
photographer judging an exposure is judging what the camera recorded, not what
this display can show: measured after the profile, the same photograph's
histogram would move when the window was dragged to another monitor, and would
report clipping belonging to the screen rather than to the picture. See
[ADR 0019](https://github.com/lacodda/nitid/blob/main/docs/adr/0019-the-histogram-counts-the-file-not-the-display.md).

The count is not on the way to the first pixel. A file nobody has asked to
measure is never measured, and when you do ask, the counting runs on a worker
thread over the pixels the loader is already holding — nothing is decoded
twice. A large photograph is sampled rather than counted whole: the shape of a
sixty-megapixel frame is settled long before the last pixel.

**The loupe**, by holding `Z`: 100% under the cursor for as long as the key is
down, and the framing you had back the moment you let go. It answers the one
question a fitted photograph cannot — is this actually sharp — without the pan
in, zoom, and pan back that asking it otherwise costs. Because it is held
rather than toggled there is no mode to be left in: stepping to the next image
while it is down carries the framing underneath it, not the loupe's, and a key
let go while another window is in front drops it too.

## Colour tools


Three things that answer questions about colour rather than about the picture.

**The clipping zebra**, with `G`: diagonal hatching over the pixels that hit
the ends of the scale — red where a highlight is blown, blue where a shadow is
blocked. It marks **what the file clipped**, not what this display cannot
reproduce, which is the same decision the histogram is built on
([ADR 0019](https://github.com/lacodda/nitid/blob/main/docs/adr/0019-the-histogram-counts-the-file-not-the-display.md)): a
colour outside this monitor's gamut is not a colour the camera lost. The
marking happens in the shader that was going to run anyway, so turning it on
and off costs one uniform write and no re-decode.

**The eyedropper**, with `C`: the colour under the pointer, in both the terms
that matter — the numbers the file holds, and what they become on this display.
They differ whenever the image carries a profile the display does not share,
and a viewer reporting only one of them would be answering a question nobody
asked. Clicking copies the file's value as hex, because that is the one that
stays true when the window moves to another monitor. Above the numbers sit the
nine-by-nine pixels around the one being read, magnified, with that one marked:
a single pixel is a number, and its neighbours are what say whether the number
is the colour of the thing or a speck on it — and, at a zoom where a pixel is
smaller than the pointer, which pixel is being read at all. The plain swatch is
still there as a setting, for anyone who wants the panel small.

**The colour passport**, with `K` or by clicking the colour in the status line:
what the file says its numbers mean, what the display says it can show, and
what is being done between them — including anything the viewer could not
honour. A HEIC that describes itself with wide primaries or an HDR transfer is
resolved to sRGB inside the decoder before a pixel reaches the viewer, so the
colour on screen is right for sRGB and wrong for the file; the passport says
so, and so does the save box, because a colour that is quietly wrong is the one
kind of wrong nobody can find by looking. Colour management is invisible when it works
and inexplicable when it does not — a photograph that looks wrong here and
right elsewhere is a question nobody can answer by looking harder at it.
