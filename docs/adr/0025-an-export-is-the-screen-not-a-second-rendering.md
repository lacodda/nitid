# 0025. An export is the screen, not a second rendering

## Status

Accepted, v0.30.0.

## Context

`Ctrl+Shift+S` writes the picture on screen as a JPEG, PNG or WebP. The
question a viewer has to answer before it can do that is what "the picture"
means: the numbers the file stores, or the light the screen shows.

They differ whenever the file states a colour the display cannot reproduce
directly — a wide-gamut photograph, and above all an HDR one, whose whole point
is light brighter than the screen's white.

## Decision

**The export runs the shader's colour path, step for step.** The same tone
curves, the same 3x3 matrix between primaries, the same clamp, the same sRGB
encoding — on the CPU over every pixel rather than on the GPU over the visible
ones. `ColorTransform::to_display` is the matrix half, shared rather than
copied; `holds_the_same_colour_as_the_shader` reads `shader.wgsl` itself and
fails when the two stop agreeing.

**An HDR picture is converted the way it is shown on a standard-range monitor
and no better.** PQ reference white — 203 nits, BT.2408 — lands on white
through the scale that already rides in the matrix (ADR 0014), and light above
it clips. There is no tone-map operator, no knee, no shoulder.

This is a deliberate loss, and the box says so before writing anything.

## Why not a tone-map operator

A Reinhard or BT.2390 curve would keep the highlights a clip throws away, and
the exported file would look better than the picture it was exported from. That
is the objection, not a side effect: a viewer's export is trusted because what
arrives is what was seen. A file that is quietly improved on the way out means
the preview was never the thing being sent, and the next question — why does
the screen not show what the file has? — has no good answer.

The operator is worth having. It belongs in the renderer first, where it
changes what a person sees, and the export follows it there. Until then, both
halves clip, and they clip identically.

The gate holds this rather than trusting it: a mid-tone is asserted alongside
the clipped highlight, because an operator earns highlights by darkening
mid-tones. Inserting one fails the test on reference white.

## Why not PNG 16-bit, JXL or AVIF as an HDR output

Considered and dropped from this version. `DecodedImage` carries eight- or
sixteen-bit integer samples and no floating-point path, so a PNG would carry
the PQ signal in a container almost nothing reads as HDR — the same numbers,
with the meaning lost. JXL and AVIF cannot be written at all here: `jxl-oxide`,
`rav1d` and `avif-parse` all decode only, and the one JXL encoder in the tree
is a test-only AGPL dependency for exactly that reason.

An HDR output needs an encoder and a licence decision of its own. It is not
this version.

## Consequences

- A save can be trusted to match its preview, and a gate fails when it stops.
- HDR highlights are lost on export. Said plainly in the box, in the README and
  here; not softened by a rendering the screen does not do.
- The three targets need no new dependency: `image` encodes PNG and JPEG,
  `image-webp` carries `WebPEncoder`.
- Nothing is overwritten. A name that already exists is refused rather than
  replaced, because the name is typed and a collision is more likely to be a
  mistake than an intention.
