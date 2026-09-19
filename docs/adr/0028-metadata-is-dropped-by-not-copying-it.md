# 28. Metadata is dropped by not copying it

Date: 2026-09-19

## Status

Accepted. Extends ADR 0027 from cropping to the rest of the container, and
settles the question ADR 0024 raised about who owns a file's orientation.

## Context

Sending a photograph to somebody sends its EXIF, and with it the camera, the
date and usually the place it was taken. The viewer could already read all of
that (`I` shows it); this version is about removing it.

There are two ways to produce a file without metadata.

## Decision

**Walk the container and do not copy the parts that describe rather than
depict.** No encoder runs. A scrubbed JPEG is identical to the original from its
start-of-scan marker onward, byte for byte, which the tests assert on the bytes.

The alternative is to hand the decoded picture to the export path, which writes
through the `image` crate and emits no EXIF at all — so "save as" already
produces a clean file, and the feature would have been two lines. It was
rejected because it re-compresses the photograph. A viewer whose whole promise is
an honest picture must not be the program that degrades one, and "you can send
this without telling them where you live, at the cost of the image quality" is
not the trade the owner asked for. The same reasoning as ADR 0027, applied one
level out from the entropy stream.

**The list of what counts as metadata lives in one place.** `src/scrub.rs` owns
it, per format, and classifies by payload signature rather than by marker:
`APP1` is EXIF *or* XMP depending on the string that follows it, and `APP2` is
the colour profile *or* something a camera maker invented. A segment whose
signature is not recognised is kept — dropping every unknown `APP` would mean
removing parts of files this cannot describe.

That one place is also why the crop is not a second implementation.
`jpeg_lossless::write` already walked the same segments and carried all of them
across; the v0.31.0 pitfall about a second write path not inheriting the first
path's warnings is exactly this shape, and the answer is one opinion about what a
segment *is*, consulted by everything that writes a file.

**The colour profile is not metadata, and stays by default.** It says nothing
about the owner, the camera or the place; it says what the numbers mean. A
picture whose profile is removed is not anonymous, it is wrong — its numbers now
claim to be sRGB when they are not, and every reader will be wrong in the same
direction. `Keep::Nothing` exists for a picture whose colour has already been
baked in by `Ctrl+Shift+S`, and it is opt-in.

**The orientation is baked into the pixels rather than dropped.** It is the one
field that is both metadata and load-bearing: remove it from a photograph taken
sideways and the picture is shown sideways ever after. ADR 0024 chose to write a
turn *as a tag* precisely because permuting a JPEG's blocks meant a C library.
That reason expired when v0.31.0 built the coefficient level in pure Rust, so
the turn is now available as pixels too — and ADR 0024 stands unchanged for what
`Ctrl+S` does, because turning a photograph to look at it should still not
rewrite it.

The bake is the crop's mechanism with one more step. A transpose of a block of
pixels is a transpose of its coefficients; a mirror is a sign change on the
coefficients whose index along the mirrored axis is odd. Every one of the eight
symmetries is a composition of those two, and no step changes a coefficient's
magnitude.

Three things about it are worth recording because each was wrong first and each
was invisible:

- **The quantisation tables must be transposed with the coefficients.** A table
  is a quantiser per frequency *position*. Move coefficient `(u, v)` to `(v, u)`
  and leave the table alone, and every coefficient is divided by its
  neighbour's quantiser. The tables encoders write are not symmetric about the
  diagonal, so this is not theoretical: it measured 15 of 255, on every pixel,
  while the geometry looked perfect.
- **Each component's sampling factors travel with the axes.** 4:2:2 is sampled
  2x1, and a quarter turn makes it 1x2. A header left stating the old pair
  describes a block order the scan no longer has.
- **The direction is described once.** `Turn::parts` gives one transpose and two
  mirrors, and both the grid of blocks and the coefficients inside each block are
  derived from it. Two hand-written tables are two chances to turn one way in one
  place and the other way in the other — which produces a picture that is nearly
  right, passes every reversibility test (four turns the wrong way round come
  back just as well as four the right way) and is caught only by comparing
  against an ordinary rotation.

**A turn that would bring a partial edge inside the picture is refused.** A
JPEG's last block in a row is partial, and the padding past the edge is harmless
while the edge stays an edge. The viewer keeps the orientation tag in that case
and says which of the two it did, rather than producing a seam.

**The clean copy goes beside the original, never over it.** A scrub is not an
edit of the photograph: it makes the version that goes to somebody else, and the
one that stays is the one with the camera and the date in it. Overwriting would
be the viewer deciding that the owner no longer wants their own metadata.

## Consequences

The viewer now has a third path that writes an image file, after the export and
the crop, and the reason the list of metadata is shared rather than copied is
that there will be a fourth.

HEIC, AVIF and JPEG XL cannot be scrubbed. Their metadata sits in a box tree
whose offsets point into the picture data, so removing a box means rewriting
those offsets, and getting that wrong produces a file that no longer decodes.
The action is greyed out with the reason rather than attempted.

`Ctrl+M` writes to the user's folder without asking for a name. That follows the
crop: the box is already the place where the decision is made, and a second box
on top of it to type into would turn one action into a form. The name is derived
and collisions are numbered.
