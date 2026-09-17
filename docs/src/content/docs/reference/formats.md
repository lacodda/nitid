---
title: Formats
description: Every format nitid opens, and what it does with each.
---

Everything below decodes in pure Rust: a malformed file costs an error, never
code execution.

| Format | Extensions | Embedded ICC profile |
| --- | --- | --- |
| JPEG | `.jpg` `.jpeg` `.jpe` `.jfif` | yes |
| PNG | `.png` | yes |
| WebP | `.webp` | yes |
| JPEG XL | `.jxl` | yes |
| HEIC | `.heic` `.heif` `.hif` | yes when embedded; code points converted at decode |
| AVIF | `.avif` | yes, from the bitstream |
| SVG | `.svg` | drawn in sRGB |
| GIF | `.gif` | sRGB by definition |
| BMP | `.bmp` | no |
| TIFF | `.tif` `.tiff` | no |

The format is decided by the bytes, not the extension: a `.png` that is really
a JPEG opens rather than erroring.

GIF, APNG and animated WebP **play**: every frame is decoded up front, the
space bar pauses and resumes, and the title carries the frame counter. Frame
delays of 10 ms and under are read as 100 ms — the convention browsers apply,
which the files were written against. A still image costs no GPU time and no
wakeups; a playing animation wakes the event loop for its next frame and for
nothing else, so pausing restores the silence.

HEIC — the format a modern phone photographs in — decodes in Rust like the
rest, container and HEVC alike, and reaches the screen as fast as a JPEG: it
carries a thumbnail as a second image inside its container, and nitid shows
that first while the full picture decodes behind it. A 10- or 12-bit HEIC
keeps its depth: the decoder hands over sixteen-bit samples and they reach the
texture that wide.

One limitation remains, and only for some files. A HEIC states its colour
either as a set of standard code points or as an embedded ICC profile. With a
profile, nitid reads it and applies it on the GPU like every other format.
With code points — the more common case — the decoder resolves the colour
itself before nitid sees the pixels, so a photograph tagged Display P3 is
shown inside sRGB rather than across a wide-gamut display's full range. See
[ADR 0007](https://github.com/lacodda/nitid/blob/main/docs/adr/0007-heic-decodes-in-rust.md).

AVIF decodes through `rav1d` — dav1d translated to Rust by the ISRG — with the
container read separately. It gets the full colour treatment: the file's
primaries and transfer curve are read from the bitstream and applied on the GPU
like every other tagged format, so a Display P3 AVIF is shown across the
display's gamut rather than folded into sRGB. 10- and 12-bit AVIF decode at
their own depth: the samples cross the whole pipeline — decoder, sandbox,
texture — sixteen bits wide, never narrowed to eight. An HDR10 file (BT.2020
with the PQ transfer) shows at a sensible brightness on both kinds of display:
its reference white lands on SDR white, and on an HDR display the highlights
above it drive the panel's headroom. See
[ADR 0008](https://github.com/lacodda/nitid/blob/main/docs/adr/0008-avif-decodes-with-rav1d.md) and
[ADR 0014](https://github.com/lacodda/nitid/blob/main/docs/adr/0014-pq-reference-white-lands-on-sdr-white.md).

HEIC and AVIF decode in a **separate process** — not because they are unsafe
any more, but because a process can be stopped and a thread cannot. Navigate
away from a large image and the decode is abandoned rather than finished for
nobody; hand the viewer a file that wedges a decoder and the child is killed on
a timeout rather than taking a worker with it. The child is created suspended
inside an **AppContainer with no capabilities**, held in a job object that caps
its memory and kills it with the viewer, and handed the file's bytes on stdin
and never its path; the pixels come home through shared memory. See
[ADR 0009](https://github.com/lacodda/nitid/blob/main/docs/adr/0009-heavy-decodes-run-in-a-child.md) and
[ADR 0011](https://github.com/lacodda/nitid/blob/main/docs/adr/0011-the-decoder-loses-the-network.md).

The container is what closes the **network**, and closed is measured rather
than assumed, in both directions: a decoder taught to try cannot reach a live
listener waiting just outside the sandbox, and a listener it binds inside
accepts nothing while the same test hammers the port from outside. The
previous arrangement — a restricted token at low integrity — demonstrably
does not close a socket, whatever the common belief; it remains only as the
fallback for a machine that cannot register a container profile, and falling
back is reported rather than silent.

SVG is drawn for the size it is shown at, and drawn again when that changes, so
zooming in sharpens the picture instead of enlarging pixels. A document that
references another file does not get one: nitid refuses every href that is not
embedded, because an image is untrusted input and must not choose what the
viewer reads off the disk. Compressed `.svgz` is not opened — decompressing it
has no size limit to hide behind.
