# 24. A turn is saved as a label, not as new pixels

Date: 2026-09-10

## Status

Accepted. Settles the question ADR 0018 left open.

## Context

Turning a picture in the viewer moved only the view: step to the next image
and back, and the turn was gone. ADR 0018 said writing one back was a later
version, and this is that version.

"Without loss" is the whole promise, and there are two ways to keep it.

## Decision

**Rewrite the EXIF orientation tag; never touch the pixels.** The file's
compressed data is not read, not decoded and not re-encoded — a saved JPEG is
byte-identical from its start-of-scan marker onward, which a test asserts on
the bytes rather than on a decoded picture. "Without loss" here is not a claim
about a careful encoder; there is no encoder in the path at all.

The alternative is permuting the JPEG's MCU blocks, the way `jpegtran -rotate`
does. It was rejected on three counts, and the first alone would have been
enough:

- In Rust it means `turbojpeg`, a C library. Every decoder here is pure Rust on
  purpose — ADR 0002 and ADR 0007 record what the C paths cost (vcpkg, meson or
  libclang, and in one case downloading a binary during the build).
- It works for JPEG only. The tag serves JPEG, PNG, HEIC, WebP and TIFF alike,
  so the cheaper mechanism is also the broader one.
- Edges that are not a multiple of the block size are trimmed or degraded.

The price is real and is not hidden: **a program that ignores EXIF shows the
picture the way it was.** Every browser, phone gallery and viewer worth the
name honours the tag, and a viewer that silently re-encoded a photograph to
satisfy the ones that do not would be trading the promise for the exception.

**What is written is the composition, not the turn.** The file already carries
an orientation; the person turned the picture *as they saw it*, which is that
orientation with their turn on top — the same composition the renderer draws
(ADR 0018). Writing the turn alone would discard what the file said, and would
look correct on every photograph whose file said `Normal` — most of them — so
the defect would ship and surface only on pictures from a phone held sideways.

**Saving folds the turn into the orientation and clears it.** The picture does
not move: it is the same composition, described differently. But a second press
then writes the same value rather than turning further, and stepping away and
back shows what was saved.

**Turning and saving stay separate keys.** `R` and `Shift+R` turn the view,
`Ctrl+S` writes it. Looking at a photograph from another angle should leave
nothing on disk; "turned it to look" and "turned it for good" are different
intentions and a viewer must not conflate them silently.

## Consequences

The viewer now writes to the user's image files. That is a line worth naming:
until this version it only ever read them, and everything that moved a file
went through the shell (ADR on file operations). The write is as small as a
write can be — one tag — but the file's bytes change and its modification time
moves.

The cached decode is dropped after a save, because the viewer itself made the
cache stale. Without that, stepping away and back would show the picture as it
was before the save, with nothing to say which was right.

Formats that cannot carry EXIF fail with a message rather than appearing to
work. A viewer that said it saved a turn and did not is worse than one that
admits the format has nowhere to put it.
