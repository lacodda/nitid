---
title: The interface
description: The status line, the toolbar and the key sheet - none of it in front of the photograph.
---

The chrome is not there while you are looking at a photograph. There is a
status line along the bottom saying what is on screen — the file, where it sits
in the folder, its size, format, bit depth, what the colour transform is doing,
and the zoom — and everything else appears when you reach for it.

Move the pointer to the top of the window and a toolbar comes down. It is the
top bar every product of the lacodda line opens with: the mark and the name on
the left, then where the file lives as a trail of folders — the ones nearest
the file stay when the window is narrow, and the full path is in its tooltip —
and the actions on the right: step
through the folder, zoom, fit, actual size, turn, the zoom lock, the backdrop,
the Info panel, the histogram, the clipping zebra, the eyedropper, full
screen. It carries nothing the keyboard does not, and every button names its
key. Press `?` for the full list.

**It is not on the way to the picture.** Laying the interface out and building
its place on the GPU costs around forty milliseconds, so the first frame is the
photograph alone and the chrome arrives on the frame after — measured at 44 to
86 milliseconds behind it, and held in that order by a test rather than by
intent. The startup promise is unchanged: first pixels in 407 to 509
milliseconds on the same file that took 489 to 528 before the interface
existed.

## Light, dark and the line's colours

The chrome follows Windows: a light theme when apps are set to light, a dark
one otherwise, switching the moment the setting changes. Both are the colours
of [dowel](https://github.com/lacodda/dowel), the design system the line's
products share, resolved for nitid's cyan — the same surfaces, lines, text
greys and control radius as the rest of the line.

Two things are deliberately not themed. The grey behind the photograph stays
neutral in both themes, because a tinted backdrop shifts how the picture's own
colours are judged. And marks drawn on the photograph — the crop frame, the
eyedropper — stay black and white, the way a camera's focus frame is, so they
read on any picture.

There is still no splash screen: the window stays hidden until the first frame
of the photograph is ready.

Drawing it correctly on an HDR surface took a detour worth knowing about. egui
picks how to encode its output from whether the target is an sRGB format, and
the extended-range surface is not one — drawn straight onto it, a mid grey came
out 2.35 times too bright. So egui draws into an sRGB texture of its own and a
shader of nitid's composites that onto the surface, asking the same question
the image shader asks. The interface also stops at SDR white: a toolbar pushed
into the display's headroom would compete with the photograph. See
[ADR 0017](https://github.com/lacodda/nitid/blob/main/docs/adr/0017-the-interface-is-composited-through-our-own-shader.md).

Nothing here polls. A frame is laid out only when it would look different from
the last one, and the only thing that asks the loop to wake is a message while
it is fading.

## What the file says


Press `I` and a panel comes down the right-hand edge with everything the file
has to say: its size, format, bit depth and colour, its weight on disk and
where it lives, and — for a photograph — what the camera wrote. Make and model,
lens, shutter speed, aperture, ISO, focal length with its 35 mm equivalent, and
when the picture was taken. A photograph carrying GPS gets its coordinates.

It is an overlay, so the picture keeps its framing while the panel is up.

**Every row copies its value when clicked.** A lens name, a shutter speed or a
coordinate is nearly always wanted somewhere else — a caption, a search, a map
— and reading it off the screen to type it back in is the part that wastes the
panel.

Values are shown the way a photographer reads them, not the way the standard
stores them: `1/250 s` rather than 0.004, `f/2.8` rather than 28/10. A maker
that repeats itself in the model is printed once.

The coordinates stay on your machine. nitid does not open a map, or ask any
service where a photograph was taken — a viewer that reached out to place your
pictures would be telling somebody else where you have been.
