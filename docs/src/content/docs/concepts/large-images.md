---
title: Large images
description: What happens when an image is bigger than a GPU texture can hold.
---

A GPU texture has a maximum side — 16384 on current integrated hardware, and
as little as 2048 on the oldest cards nitid still runs on. A stitched panorama
or a scanned map goes past it, and the way that failed is worth knowing: the
graphics API has no way to return the rejection, so it reports it through a
side channel and the default handling is a panic. Before this version such a
file decoded all the way through and then took the viewer down at the moment
it was about to appear.

nitid cuts such an image into tiles the device will hold and draws them as one
picture. Zoom and pan work as they do on any other image, and the joins are
invisible: each tile carries one pixel of its neighbour so the filter has a
real texel to interpolate towards, rather than the repeated edge that leaves a
visible step under magnification. An image that fits in one texture is still
one texture and one draw call — tiling costs it a single comparison.

Still bounded by memory rather than by the texture limit: a tiled image holds
every pixel at once. See
[ADR 0015](https://github.com/lacodda/nitid/blob/main/docs/adr/0015-large-images-are-tiled.md).
