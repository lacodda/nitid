# 18. Orientation composes by matrix multiplication, in an order that was measured

Date: 2026-08-28

## Status

Accepted.

## Context

The viewing rotation (`R`) has to combine with whatever the file already asks
for. A HEIC that states `Rotate90` in its container, turned once clockwise by
the user, has to end up at `Rotate180` — and the mirrored orientations, which
real files do carry, have to be right too.

The obvious implementation is a table: eight rows saying what follows what,
twice, one table per direction. It is also how the equivalent geometry went
wrong in v0.15.0, where a sign error in the tile orientation passed every
arithmetic invariant and was caught only by comparing rendered frames.

The eight orientations are the dihedral group of order 8, and `gpu.rs` already
held them as 2x2 matrices to hand to the shader. Composition is therefore
matrix multiplication, and needs no table at all.

## Decision

**The eight matrices are stated once, on `Orientation`**, and `gpu.rs` reads
them rather than keeping its own copy. Two copies would be two places for a
sign to drift, and the drift would only show on mirrored orientations that no
ordinary photograph carries.

**The composition order is `self * then`, and it was derived rather than
chosen.** The matrices map quad corners to *texture* space, so they are the
inverse of what the picture visibly does, and the two candidate orders —
`M * R` and `R * M` — are not interchangeable:

| Starting orientation | `M * R90` | `R90 * M` |
| --- | --- | --- |
| Normal | Rotate90 | Rotate90 |
| Rotate180 | Rotate270 | Rotate270 |
| **FlipHorizontal** | **Transverse** | **Transpose** |
| **Transpose** | **FlipHorizontal** | **FlipVertical** |

They agree on every rotation and disagree on every mirror. A table written by
looking at photographs would therefore be right in all the cases anyone tests
and wrong in the four that only appear in files from a scanner or a phone that
mirrors its front camera.

The answer came from tracking a texel: take the top-left texel, find where it
lands on screen under each orientation, turn that screen point a quarter
clockwise, and ask which orientation puts the texel there. Two probe points
pick exactly one candidate, and it is `M * R`.

## Consequences

`turned` is one line, `then` is a 2x2 multiply, and there is no table to get
wrong. The properties that hold it down are the ones a group has rather than
ones an author thinks of: four quarter turns return to the start from any
orientation, a turn and its opposite cancel, a quarter turn exchanges the axes
and a half turn does not, and every product of two orientations is one of the
eight — which is what lets the lookup return a value rather than a fallback.

A property suite alone would not have caught a swapped direction: turning the
wrong way still returns after four turns, still cancels, still exchanges the
axes. One example test states which way `R` goes on an upright picture, and
mutation testing confirms it is the only thing that notices when the direction
is reversed.

The user's turn is not written to the file, and does not survive a step to
another image. Writing a rotation back — losslessly for JPEG — is v0.26.0.
