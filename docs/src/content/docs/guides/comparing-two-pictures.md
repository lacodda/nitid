---
title: Comparing two pictures
description: Pinning one frame, walking a burst beside it in step, blinking the two, and reading what the camera did differently.
---

Choosing between the frames of a burst is rarely a question about one pair. It
is a walk through the series against whichever frame is best so far — the eyes
open in this one, the horizon straight in that one — and a viewer that can only
show one picture at a time turns that walk into memory work.

**`V` pins the picture on screen and puts the next one beside it.** The pinned
picture stays where it is; the arrow keys walk the other one through the
folder, so every frame of the burst meets the one to beat. On the last picture
of a folder that does not wrap, the comparison opens with the picture before
instead.

**`Enter` makes the right-hand picture the one to beat.** It moves into the
pinned place and the walk goes on to the next frame — which is the whole of
choosing from a burst: pin the first, walk, `Enter` whenever something beats
it, and what is pinned at the end is the pick. The comparison never marks or
moves a file on its own.

**`V` again, or `Esc`, ends it on the pinned picture** — framed exactly as it
was, turned the way you turned it, and without decoding it again. After a walk
through a burst that is the winner, so the next key is `P`; after a quick look
at the next frame it is simply where you were. The arrow keys carry on from
there.

**The two are framed as one.** Zoom, pan, `0`, `1` and the loupe on `Z` act on
both at once: the pinned picture always shows the same part of the scene as
the other, at the same size on screen, so checking whether an eye is sharp
means zooming once, not twice. The wheel zooms around the point under the
pointer in whichever pane it is over, and that point stays put in both.
Stepping to the next frame keeps the framing, as if the lock on `L` were on —
otherwise the pinned picture would jump back to fit under the eye on every
arrow key.

The framing is matched by what is shown rather than by pixel scale. Two frames
of one burst are the same size and come out identical either way; the
difference is a cover exported at 2000 pixels beside its 4000-pixel original,
where holding the pixel scale would show twice as much of one as of the other.
Matched by content, the two show the same region at the same size, and the
smaller one is simply softer — which is usually the question being asked.

**The window splits the way that shows the pinned picture larger.** Portrait
frames go side by side, a panorama goes one above the other. The split is
decided by the pinned picture, so it holds still while the other one is walked
through frames of any shape.

**`Shift+V` blinks them instead.** Both pictures take the whole window, one at
a time, changing every half second. A difference that hides side by side —
an eyelid half closed, a horizon a few pixels lower, a crop nudged — jumps out
when the two alternate in one place, because the eye notices change far better
than it compares. The tag in the corner says which picture is up at the
moment. `V` switches back to side by side, `Shift+V` again stops. While a
blink runs, the viewer wakes for its next change and for nothing else; stop it
and the window is as idle as it was.

**The table between them says what the camera did differently.** Only the
fields that differ, and on one line underneath, the ones they share — so "the
same ISO" is still an answer. The last column says how far apart two values
are:

- **Exposure, aperture and ISO in stops of light**, measured from the pinned
  picture to the other: `+1 EV` means the right-hand frame was given a stop
  more light, whether that came from the shutter, the aperture or the ISO. The
  three read the same way, so a bracketed series reads as one scale.
- **The moment in time**, down to the fraction of a second a camera keeps
  beside the date: a burst of ten frames a second happens inside one second,
  and `+0.40 s` is the difference between the fourth frame and the eighth.
- **Size, format and file size**, for two exports of one picture.

**The tools read whichever picture they are over.** The eyedropper names the
pixel under the pointer in the pane the pointer is in, with that picture's own
colour profile — two variants of a cover are compared by colour as often as by
shape. `Ctrl+Drag` hands over the file of the pane the drag starts in. Each
pane is drawn through its own profile too: a photograph in Display P3 beside
its sRGB export shows each one correctly, in the same frame.

**Everything that changes a file acts on the right-hand picture** — the one the
status line names. `P`, `X` and `U` mark it, `Del` and the sorting keys move
it, `F2` renames it. The pinned picture is a reference and nothing touches it
until the comparison ends on it. A crop needs the whole window and one
picture, so `Ctrl+X` leaves the comparison and stays on the right-hand picture
— the one it will cut.
