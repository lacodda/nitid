# 20. A pasted picture is shown, not saved

Date: 2026-09-01

## Status

Accepted.

## Context

`Ctrl+V` puts whatever bitmap is on the clipboard on screen. That bitmap has no
file behind it, and nearly everything else in the viewer is built on the
assumption that there is one: the title bar names it, the arrow keys walk its
neighbours, the loader caches decoded pixels by path, the eyedropper and the
histogram look their pixels up the same way, and `Ctrl+Shift+C` copies it.

The obvious way out is to write the bitmap to `%TEMP%` and open that. Every
feature then works with no special case, the code stays simple, and the paste
behaves exactly like an open.

## Decision

The pasted picture is held in memory and shown as itself. Nothing is written to
disk. Decided by the owner, 2026-09-01.

It is marked as pasted — its own flag on the shown image, not an invented path
— and the places that name a file answer accordingly: the title and the status
line say `clipboard`, the folder is dropped so the arrow keys have nowhere to
go, `Ctrl+Shift+C` says there is no path, and the tools that read pixels take
them from the one copy the application holds rather than from the loader's
cache.

## Consequences

- **The viewer does not write to disk unasked.** A photograph viewer that left
  files behind every time somebody pressed `Ctrl+V` would be doing something
  the person did not ask for, in a place they would not think to clean.
- **A paste is not an open.** The folder is dropped rather than kept: leaving
  it would let an arrow key replace the pasted picture with a neighbour of the
  file that happened to be open before, which is a surprising way to lose
  something that exists nowhere else.
- **Saving a pasted picture is a separate feature**, and belongs with the rest
  of exporting (v0.27.0) where the person names the file.
- **The special case is real and has to be carried.** Four places ask "is there
  a file here" and each had to be taught the answer. The alternative hid that
  cost behind a temporary file and paid it in surprise instead.

## Why not the temporary file

It is the cheaper implementation and the worse behaviour. The cost of the
decision is a flag threaded through four call sites — visible, testable, and
paid once. The cost of the temporary file is invisible: files accumulating in
`%TEMP%` from an application nobody would suspect of writing any, and a paste
that silently commits a picture to disk before the person has decided they want
to keep it.
