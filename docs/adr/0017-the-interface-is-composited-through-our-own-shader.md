# 17. The interface is drawn into a texture and composited by our own shader

Date: 2026-08-26

## Status

Accepted.

## Context

nitid needs a status line, a key sheet and toasts, and egui is the sensible way
to get them: ADR 0001 already anticipated it, on the condition that egui draws
*into* nitid's render pass rather than owning the surface, because the
swapchain has to stay ours for HDR.

Letting egui draw straight onto that surface turns out to be wrong, and by a
wide margin. `egui-wgpu` picks its fragment entry point from whether the target
format is an sRGB one (`renderer.rs:406`): an sRGB target gets
`fs_main_linear_framebuffer`, which converts gamma to light; anything else gets
`fs_main_gamma_framebuffer`, which writes the gamma value as it stands.

nitid's HDR surface is `Rgba16Float` with `ExtendedSrgbLinear` — not an sRGB
format — so egui takes the second path and writes gamma-encoded numbers into a
surface that means them as linear light. Measured, with a mid grey (sRGB 128)
read back as linear light:

| Target | Read back | Correct? |
| --- | --- | --- |
| `Bgra8UnormSrgb` — the SDR surface | 0.21586 | yes (0.2140 expected) |
| `Rgba16Float` — the HDR surface | **0.50244** | no, 2.35× too bright |
| `Rgba8UnormSrgb` — a texture | 0.21586 | yes, to the digit |

Telling egui the target is an sRGB format while drawing into a float one does
not work either: wgpu rejects the pipeline outright — "Render pipeline targets
are incompatible with render pass".

## Decision

**egui draws into an `Rgba8UnormSrgb` texture of its own**, which is where its
own choice of entry point is the correct one, and a second pass composites that
texture onto the surface with a shader of nitid's.

That shader asks exactly the question the image shader asks, and answers it the
same way: light for an extended-range surface, an sRGB encoding for a surface
that does not encode on write, and untouched values for one that does. A single
`SurfaceUniform` carries the answer, built by the same rule as `ColourUniform`,
and a test holds the two rules against each other.

**The interface is not pushed into the display's headroom.** On an
extended-range surface 1.0 is SDR white and the shader stops there. A toolbar
brighter than white would compete with the photograph, which is the thing worth
looking at.

**A frame is laid out only when it would look different.** The interface keeps
a digest of what it last showed; an unchanged status is not laid out and not
drawn. Nothing here polls, and the only thing that asks the loop to wake is a
toast, while it is fading — because a fade has to be drawn to be seen.

**The interface waits for the picture.** Nothing is laid out until a frame
carrying an image has been on screen once, and the chrome arrives on the frame
after. This was measured rather than assumed, and the numbers decided it:

| | Before the interface | Drawn on the first frame | Drawn on the frame after |
| --- | --- | --- | --- |
| First pixels | 489–528 ms | 550–576 ms | 407–509 ms |

Building the compositing layer alone accounts for 64–69 ms of that, and laying
a frame out for a further 10–14. Deferring construction to the first draw was
not enough on its own — the status line is always showing, so it was always
built — and the order is what actually removed the cost. The chrome follows
44–86 ms behind the picture, which is below the threshold at which a person
reads two events as separate.

It is the same order the viewer already uses for an embedded thumbnail: put
something true on screen, then improve it.

**The toolbar appears on approach and leaves.** It is shown while the pointer
is within 64 logical points of the top of the window, and stays while the
pointer is over the toolbar itself — otherwise moving onto a button would take
the button away. It carries nothing the keyboard does not, and a button does
not act: it names the key it stands for and the key handler does the rest.

That indirection is the answer to a real defect, found by mutation. With the
button wired to a `match` of its own, swapping two arms — the back button
stepping forward — passed every test and the whole gate: the toolbar worked,
differently from the keyboard, and nothing could see it. Naming a key instead
leaves something a test can hold: every action must resolve to a key the viewer
answers, and no two may resolve to the same one.

## Consequences

The interface is correct on both surfaces, and the picture underneath is
untouched: the image pass is what it was, and the interface is a second pass
that loads rather than clears.

It costs one full-window `Rgba8UnormSrgb` texture and one extra draw call per
frame that shows chrome. That is paid only when there is chrome to show, and
never before the photograph is up.

The startup gate grew a third test for the order, because the other two could
not see it: both run with `NITID_EXIT_AFTER_FIRST_FRAME=1`, and a viewer that
quits on the first frame never draws the second. The variable therefore takes
`interface` as well, which waits one frame longer — and the test asserts both
halves, that the chrome is not in front of the picture and that it does arrive
behind it. A timing check alone would pass on an interface that was never
drawn at all.

The compositing pipeline is built against the format it writes, so it is
rebuilt when the surface moves between standard and extended range — alongside
the image pipeline, in the same place, for the same reason. The egui renderer
itself is not rebuilt, because it always draws into the same texture.

The MSRV moved from 1.89 to 1.95. `egui-wgpu` 0.36 is the first release built
against wgpu 30, and it requires 1.95; the alternatives were holding wgpu back
to 29 — undoing measurements from three versions — or drawing every widget by
hand. This is a promise to anyone building from source, so it is stated in the
manifest, checked by CI against the manifest, and written down in the
changelog.

Not addressed: the interface is drawn at the surface's own scale, so a
fractional display scale factor lands text on half-pixels. egui handles this
itself through `pixels_per_point`, and it is fed from the window, but nothing
here measures the result. Worth a look if text ever reads as soft.
