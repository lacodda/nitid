# 10. The container is rewritten in a copy to steer the decoder

Date: 2026-08-21

## Status

Accepted. Settles two of the consequences recorded in
[ADR 0007](0007-heic-decodes-in-rust.md).

## Context

`heif-oxide` has one entry point: hand it a HEIC, get the primary image back.
That is all a viewer needs until it wants two things the crate does not offer.

**The thumbnail.** A HEIC carries no EXIF thumbnail. What it has is a second,
small picture stored as another item in the same container, tied to the full one
by a `thmb` reference — which is what a phone writes and what the shell shows in
a folder. The crate decodes the primary item and offers no way to ask for
another. Without it, the screen waits for the whole HEVC decode: measured at
about 470 ms for a 12-megapixel photograph against 5 ms for its thumbnail.

**The colour of an ICC-tagged file.** A HEIC states its colour either as CICP
code points or as an embedded ICC profile. The crate reads the first and, for
the second, records only that a profile is present — then falls back to a
default set of matrix coefficients for the YUV conversion. That default is
wrong often enough to be visible: a flat green libheif reads as (10, 200, 90)
came back as (0, 185, 85). Not a narrower gamut — a different colour.

Both are facts about the crate rather than about the format, and both were
measured against libheif on the same files rather than reasoned about.

## Decision

For each case, the container is rewritten **in a copy** so that the decoder,
unchanged, does what is wanted:

- **Thumbnail:** the `pitm` box, which names the primary item, is rewritten to
  name the thumbnail item found through `thmb`. Two bytes.
- **ICC colour:** the `colr` box, which for such a file holds an ICC profile, is
  rewritten to hold CICP codes stating the coefficients the pixels were actually
  coded with. The profile itself is read separately and handed to the shader, so
  the colour is applied on the GPU as it is for every other tagged format.

The box walk both need lives in `isobmff`, shared with AVIF, which reads its
rotation the same way.

## Consequences

The alternative for the thumbnail was to assemble a standalone HEIC around the
thumbnail's coded data — rebuilding `iloc`, `iinf`, `ipco` and `ipma` by hand.
That is a second container *writer* to keep correct, against a two-byte edit to
a header the file already contains. For the colour there was no alternative
short of decoding HEVC ourselves.

What this costs:

- **A copy of the file per quick frame.** The bytes are already in memory, and
  the copy is made once per image rather than per frame.
- **The edits are only as good as the walk.** A file whose boxes are malformed
  yields no thumbnail and no colour correction rather than a wrong picture:
  every read is bounds-checked, the recursion is bounded, and a length that
  would not advance the walk ends it. Truncated and corrupted input is in the
  tests.
- **They are workarounds, and are marked as such.** If `heif-oxide` grows a way
  to decode a named item, or to read an ICC profile, both should go. The tests
  are written against the *behaviour* — a thumbnail smaller than the picture, a
  colour matching what libheif reads — so they will keep passing when the
  workaround is removed, and fail if the behaviour regresses.

One thing this deliberately does not attempt: a CICP-tagged Display P3 file is
still folded into sRGB by the decoder, and rewriting its `colr` box would not
help — the conversion is the crate's, and stating different codes would only
make it convert differently. That limitation stands as ADR 0007 records it.
