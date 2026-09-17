---
title: Honest color
description: What the file says its numbers mean, what the display can show, and the conversion between them.
---

An image carries a colour profile saying what its numbers mean; a display has
one saying what it can show. Most viewers ignore both and send the numbers
straight to the screen, which is why the same photo looks oversaturated in one
program and right in another.

nitid reads the profile out of the file, asks Windows what the display is, and
converts between them **in the shader** — the decoded pixels stay as the file
stored them, the conversion costs nothing per frame, and changing your display
profile costs a redraw rather than a reload.

- A wide-gamut file (Display P3, Adobe RGB) is brought into what the display
  can actually show, rather than clipped.
- A file with **no** profile is shown exactly as it is, the way Windows, the
  shell preview and every browser show it. It looks the same here as it does
  everywhere else — including in whatever tool made it. Guessing sRGB and
  converting from it would visibly wash the picture out on a wide-gamut
  display; see [ADR 0005](https://github.com/lacodda/nitid/blob/main/docs/adr/0005-untagged-images-pass-through.md).
- Arbitrary tone curves are handled by sampling them, so a scanner or camera
  profile costs the same as a simple gamma.

When the image and the display already agree, no conversion happens at all and
the hardware does the sRGB decoding for free.
