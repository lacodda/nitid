# 15. Images past the texture limit are drawn as tiles

Date: 2026-08-26

## Status

Accepted.

## Context

A GPU texture has a maximum side. On the hardware nitid is developed against —
Intel Iris Xe on DX12 — that is 16384; the downlevel floor the viewer targets
so older and integrated GPUs stay in scope reports as little as 2048. A
stitched panorama, a scanned map, or a gigapixel astrophotograph goes past it.

The failure mode is the reason this is a version rather than a nicety.
`create_texture` with an oversize extent does not return an error — its
signature has no `Result` to put one in. wgpu routes the validation failure to
the device's *uncaptured error handler* and hands the caller back a texture
object that looks valid:

```
Validation Error
  In Device::create_texture, label = 'oversize'
    Dimension X value 20000 exceeds the limit of 16384

returned a texture handle: (20000, 1000, 1)
```

What that costs depends on the handler, and the first measurement here was
misleading in a way worth writing down. A probe that installs its own handler —
as the probe for this work did — swallows the error and sees a texture that
simply draws nothing, which suggests the symptom is a blank window. The viewer
installs no handler and so gets wgpu's default, which **panics**. Measured by
opening a 20000×3000 panorama with the v0.14.0 binary:

```
nitid: full image up at 778.7 ms
nitid: colour: no conversion needed at 778.8 ms

thread 'main' panicked at wgpu-30.0.0\src\backend\wgpu_core.rs:1637:26:
wgpu error: Validation Error
  In Device::create_texture, label = 'nitid image'
    Dimension X value 20000 exceeds the limit of 16384
```

A panorama therefore did not open blank before this change: it decoded in
full, and then took the viewer down at the moment it was about to appear.

Two further facts came out of the same measurements and shaped the design:

- **The side is the constraint, not the volume.** A 16000×16000 texture — one
  gigabyte at RGBA8 — is created and written without complaint, at roughly 29 ms
  per 16 MB stripe. Only the side limit rejects anything.
- **A seam is real, and one pixel of overlap removes it.** Splitting a gradient
  across two textures and magnifying it produced a flat step at the join,
  8 values out of 255 away from the same gradient drawn whole: the sampler
  interpolates towards a neighbouring texel, and at a tile's edge
  `ClampToEdge` repeats the edge texel instead. With one source pixel of
  overlap on each interior edge, the difference is zero, at two tiles and at
  four.

## Decision

An image is cut into a grid of tiles no larger than the device's
`max_texture_dimension_2d`, each tile its own texture, bind group and draw
call. The geometry lives in `tiles.rs` as arithmetic over pixel rectangles and
is tested without a graphics device.

**Interior tiles carry one source pixel of padding on each edge that touches a
neighbour.** The padding is sampled and never drawn: `Placement` gained a UV
scale and offset naming the sub-rectangle of the tile's texture that holds the
pixels it is responsible for. The padding is counted inside the limit, so a
tile never exceeds it at the seams.

**The single-tile case stays exactly what it was**: one texture, one upload of
the decoder's buffer with no copy, one draw call, a whole-image placement with
scale 1 and offset 0. Every ordinary photograph takes that path and pays
nothing for the existence of tiling.

**Tile quads are derived from the placed image rectangle**, not computed
independently. Zoom, pan and EXIF orientation shape the whole rectangle as
before, and each tile is a fixed fraction of whatever that rectangle became —
so the framing logic exists in one place and tiling cannot drift from it.

## Consequences

A panorama opens instead of taking the viewer down, and pans and zooms like
any other image. The cost on an image that fits a texture is one comparison.

An image that must be tiled pays a copy per tile at upload, because
`write_texture` needs each tile's rows contiguous and the decoder's buffer is
one unbroken image. That is one extra pass over the pixels; the decode itself
is far more expensive.

The orientation transform had to be undone to place tiles, since a tile knows
where it sits in the image while its quad is stated in the screen's
already-rotated clip space. Getting this wrong assembles a rotated picture
inside out while every arithmetic invariant still holds, so it was caught by
comparing rendered frames rather than by reasoning about coordinates.

The undoing is a transpose: every orientation matrix is orthogonal, including
the four reflections, whose determinant is -1 and whose transpose is still
their inverse. That was measured rather than assumed — an explicit matrix
inverse was written first, and the two turned out to agree on all eight
orientations, so the simpler form stands. A test asserts the orthogonality the
transpose depends on, and another puts an offset through `to_screen` and back
through the shader's own matrix. The bug that actually reached a rendered
frame was neither: it was one negation too many on the y axis.

`NITID_TILE_LIMIT` lowers the limit so the tiled path can be exercised against
fixtures of a sane size. It can only lower it, never raise it past what the
device accepts: a texture over the device's limit is precisely the failure
this decision exists to prevent.

Two limits are deliberately not addressed. A tiled image still holds every
pixel in memory at once and uploads all of it, so a truly enormous file is
bounded by RAM rather than by the texture limit; and there is no level of
detail, so a gigapixel image zoomed out samples full-resolution textures. Both
are only worth solving behind evidence that a real file hits them.
