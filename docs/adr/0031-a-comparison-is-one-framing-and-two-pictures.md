# 31. A comparison is one framing and two pictures

Date: 2026-09-29

## Status

Accepted.

## Context

Choosing between frames of a burst, or between two exports of a cover, needs
two pictures on screen at once: side by side with their zoom and pan in step,
or blinked one over the other in the same place. Until now the renderer held
one picture, with one colour uniform and one set of tone curves shared by
everything it drew, and the view framed that picture against the whole
window.

Three questions had to be settled before any code: how two pictures share the
GPU, how their framings are kept in step, and what "in step" means when the
two are not the same size.

## Decision

**Two resident pictures, each with its own colour state.** The renderer holds
a `Main` picture — the one the arrow keys move — and a `Pinned` one. Each
carries its textures, its tone curves and its colour uniform. A shared uniform
would draw one picture in the other's colours, and a photograph in Display P3
beside its sRGB export is exactly the case a comparison is for. Pinning moves
the main picture's GPU state to the pinned slot rather than uploading it
again, so pinning a large file costs nothing.

**Panes are viewports of one pass.** A frame is a list of panes, each a
rectangle and the picture drawn in it. `draw_pane` sets the viewport to the
pane, so a placement worked out against the pane's size lands inside it, and
the scissor to the same rectangle as a guarantee. A single picture is one pane
covering the window, which is the path every frame took before.

**One framing; the other is derived.** The view of the picture being steered
is the only framing there is. The pinned picture's is worked out from it on
every frame by `View::matched`, so the two cannot drift apart — there is
nothing to keep in sync. Zoom, pan, fit, 100% and the loupe all act on the one
framing.

**Matched by content, not by pixel scale.** The pinned picture shows the same
region of the scene at the same size on screen: the zoom carried is the zoom
relative to the whole picture filling the pane (uncapped, unlike `fit`), and
the place carried is the point of the picture at the pane's centre as a
fraction of the image. Two burst frames of one size come out identical either
way. A 2000-pixel export beside its 4000-pixel original does not: holding the
pixel scale would show twice as much of one, and a blink between them would
be a jump rather than a difference.

**A pinned picture that is still a thumbnail is held, not refused.** Pinning
while the full decode has not landed asks the loader to hold that picture: a
request that no navigation makes stale, answered as `Asked::Held`. The
thumbnail stands in until it arrives.

## Consequences

- A picture can be drawn in one pane of a frame, not two: each tile has one
  placement buffer. A debug assertion says so; nothing in a comparison asks
  for more.
- The pinned picture is still: an animation pinned shows its first frame, and
  a vector image is not drawn again when the zoom changes. Both are
  deliberate simplifications of a reference picture, not of the one being
  walked.
- The window is split along the axis that shows the pinned picture larger,
  decided by the pinned picture alone so the panes do not move while the
  other is walked through frames of other shapes.
- A blink wakes the loop on its interval while it runs, the way an animation
  does, and the silence of a still picture comes back when it stops.
