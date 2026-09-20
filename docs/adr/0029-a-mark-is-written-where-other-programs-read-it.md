# 29. A mark is written where other programs read it

Date: 2026-09-20

## Status

Accepted.

## Context

Going through a shoot means saying, frame by frame, which ones are worth
keeping. The viewer is the right place to do it — it is the only view that can
answer "is this the good one" — but the answer is worth nothing if it stays in
the viewer. A selection exists to be acted on somewhere else: in Explorer, in
whatever the photographs are edited with, in a backup script.

So the question is not whether to record the mark but where. There are two
places a rating can go, and the plan for this version named both without
noticing they are alternatives:

- **An XMP file beside the picture** (`photo.xmp`), which is what Adobe's tools
  write for formats they will not modify. Nothing is written into the
  photograph at all.
- **The EXIF rating inside the file**: tag `0x4746` in IFD0, with
  `0x4749` beside it as a percentage. This is the pair Windows itself writes
  when a rating is set from Explorer's Properties dialog.

## Decision

**Into the file, as EXIF.**

This was settled by measurement rather than by argument. On Windows 11, asking
the shell for the `Rating` column of two files:

| File | Rating column |
| --- | --- |
| A JPEG carrying `0x4746 = 4` in IFD0 | `4 Stars` |
| A JPEG with no EXIF and a `photo.xmp` beside it saying `xmp:Rating="5"` | `Unrated` |

Explorer does not read sidecars. Neither do most of the programs a photograph
passes through. A sidecar is a mark only the program that wrote it can see, and
it is left behind the first time the picture is copied, attached to a message or
dragged into something else — which is precisely when the selection was supposed
to be useful.

The negative control matters and is worth recording, because the first attempt
at it was wrong: the sidecar was placed beside a *copy* that still carried the
embedded tag, and Explorer duly said `4 Stars`. A sidecar cannot be tested for
while an embedded rating is present in the same file.

**Three marks on one scale, not two independent flags.** Keep and reject could
each have been a flag of its own. They are one field with three positions
instead, because two flags mean a file has two states of judgement and "show me
the marked ones" stops having a single answer.

Reject has no EXIF representation of its own — the rating field is `0..=5` and
Lightroom's reject is a separate flag — so it is written as no stars with the
percentage at `1`. Windows never writes that pair, since every percentage it
writes comes with stars, so reading it back cannot mistake another program's
file for a rejected one. Explorer shows a rejected frame as `Unrated`, which is
the honest answer: no other program has a concept to show.

Another program's rating of two through five stars reads back as a keep. A
person who rated a photograph elsewhere has plainly said they want it, and
answering that with "unmarked" would be the viewer overruling them.

## Consequences

The file is rewritten to carry the tag. Only the metadata block is rebuilt —
never the pixels — so a marked JPEG is identical to the one that went in from
its start-of-scan marker onward, the same promise ADR 0024 makes for a saved
turn, and a test asserts it on the bytes rather than on the decoded picture.

A format that cannot carry EXIF cannot carry a mark, and says so rather than
reporting a success it did not achieve.

The mark is read from the same bytes as the rest of the metadata, on the
trusted side of the sandbox (ADR 0002): what a file *says* about itself is
settled before anything is handed to a decoder. The sandboxed path needed this
explicitly — it is the second way a file becomes a loaded image, and a fact
added to the first does not reach it by itself.

Marking is immediate. There is no pass held in memory and saved at the end: a
cull is hundreds of presses, and a selection that a crash or a closed window
loses is worse than no selection, because nothing tells the person it is gone.
