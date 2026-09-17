---
title: Controlling the view
description: Zoom, framing, rotation and the backdrop behind transparency.
---

Three things you can do to a picture without touching the file.

**Hold the framing across a step** with `L`. By default every image is framed
for itself, which is what a folder of unrelated pictures wants. Locked, the
arrow keys become a way to compare a series: each frame arrives at the same
magnification over the same part of the picture, so what moves between them is
the only thing that moves. The place is held as a fraction rather than as
pixels, so a neighbour of a different size shows the corresponding part of
itself instead of drifting.

**Turn the picture** with `R`, or the other way with `Shift+R`. It is a viewing
transform: the file is untouched, and stepping to another image shows that one
as its own metadata asks. Rotating the file itself is a later version. The turn
combines with whatever the file already asks for by multiplying their matrices,
in an order that was measured rather than chosen — the two candidate orders
agree on every rotation and differ on every mirror, so a table written by
looking at photographs would be wrong in exactly the cases photographs do not
show. See [ADR 0018](https://github.com/lacodda/nitid/blob/main/docs/adr/0018-orientation-composes-by-matrix-multiplication.md).

**Choose what shows through transparency** with `B`: the viewer's own dark
scene, a checkerboard, black, or white. Judging a cut-out against one backdrop
is judging it against one background — a logo bound for a white page has to be
seen on white, and a checkerboard is how you tell "transparent" from "a flat
grey that happens to match the scene". The checker is measured in screen
pixels, so it stays the same size at any zoom rather than reading as part of
the picture.

**The minimap** appears once part of the picture is off screen: the whole
image small in the bottom-right corner, with the part you are looking at framed
and the rest dimmed. Zoomed in, a viewer answers "what is here" and stops
answering "where is this"; the frame moves as you drag, so the photograph stays
navigable at a zoom where nothing on screen says where in the frame you are. At
a deep zoom the visible part is a hair, and the frame is held to something the
eye can find rather than drawn to scale — the zoom in the status line is what
states the measurement.

It is drawn in **the display's colours**, unlike the histogram and the
eyedropper's numbers. Those report facts about the file; a minimap is a picture
of the picture, and one painted in the file's numbers would be a visibly
different colour from the photograph it sits beside. It is built once per
image, by sampling rather than averaging, so a sixty-megapixel file costs the
same as a small one and nothing is spent inside a drag.

By default it is there only when it has something to say. The View section of
the settings has the other two answers: always, or never.
