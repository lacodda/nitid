# 21. A drag offers the file and the picture

Date: 2026-09-03

## Status

Accepted.

## Context

Dragging the image out of the window is how the viewer stops being a dead end:
the picture on screen goes into a chat, a mail, an editor without a trip back
through the file manager to find it again.

What travels has to be decided before the drag starts, because a Windows drag
offers a fixed set of formats and the receiving application picks from them.
The two candidates pull in different directions:

- `CF_HDROP`, a list of paths — what the shell itself hands over. A chat
  window, a mail client and a file manager all understand it, and what arrives
  is the original file: its own format, its own metadata, its own bit depth.
- `CF_DIB`, a bitmap — what an editor that paints understands. It carries
  pixels and nothing else: eight bits a channel, no profile, no metadata.

Offering only the path would leave the editors with nothing. Offering only the
bitmap would flatten a 16-bit HEIC into an 8-bit paste on its way into a mail,
where the file itself would have arrived intact.

A picture pasted from the clipboard (`Ctrl+V`) complicates this: it has no file
at all. The tempting fix is the one ADR 0020 already refused for a different
reason — write it to `%TEMP%` and drag that.

## Decision

Both formats are offered, `CF_HDROP` first, and the receiving application takes
whichever it understands. A pasted picture offers only `CF_DIB`.

Order matters: an application that understands both should take the file, which
is the version of the picture that loses nothing.

The pixels in the `CF_DIB` are the file's own, unconverted — the same block
`Ctrl+C` puts on the clipboard, and the same promise ADR 0019 records. A DIB
has nowhere to say what its numbers mean, so a wide-gamut picture dropped into
an application that assumes sRGB looks flatter there. That is a true statement
about the format rather than something a viewer should quietly fix by
rewriting the pixels on the way out.

Only the copy effect is offered. A drag that could *move* the file would delete
the photograph the user is looking at because they dragged it into a chat.

The gesture is `Ctrl` and a left drag, decided by the owner on 2026-09-03. The
bare left drag stays panning: it is the viewer's main gesture, and one that
changed meaning depending on the zoom would be one nobody could pan with
confidence.

## Consequences

A drop into the shell, a mail or a chat delivers the file itself. A drop into
an editor delivers the picture. Neither had to be chosen in advance, and
neither application had to learn anything about nitid.

A pasted picture can be dragged into an editor but not into a file manager,
which has nowhere to put a thing that is not a file. That asymmetry is visible
to the user and is the honest shape of the situation: the viewer will not write
to disk unasked (ADR 0020), and a temporary file would make the drag work by
doing exactly that.

`DoDragDrop` runs its own message loop, so the viewer is unresponsive for the
length of the drag. This is how every draggable application on Windows behaves
and is not worth working around: the call has to be made on the thread that
owns the window, which winit has already initialised OLE on in order to
register the window as a drop target.

The data object holds copies of the bytes rather than borrowing the viewer's
state, because an application may ask for the data after the drag has ended.
For a large photograph that is a second copy of the decoded pixels held for the
length of the drag — paid deliberately, since the alternative is a borrow the
event loop cannot promise to keep.
