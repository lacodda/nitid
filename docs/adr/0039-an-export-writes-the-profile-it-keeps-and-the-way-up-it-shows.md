# 39. An export writes the profile it keeps and the way up it shows

Date: 2026-10-22

## Status

Accepted. Corrects what ADR 0025 assumed of the encoders rather than changing
what it decided.

## Context

ADR 0025 made the export the screen: the colour the owner saw is what leaves.
Two parts of "what was seen" never reached the file, and nothing failed when
they did not.

**The profile.** "Keep profile" wrote the file's own numbers and no profile.
The encoders take one only when handed it, and none was. A Display P3
photograph saved that way arrived in every other program claiming to be sRGB,
and was shown washed out — the failure the export exists to prevent, made by
the export. The help text and the docs said the profile travelled with the
numbers; no test asked the written file whether it did.

**The way up.** A photograph taken with the phone sideways is stored sideways
with a tag that turns it. The viewer applied the tag on screen, and the save
wrote the stored pixels with no tag at all, so the saved file was sideways in
every program. The copy for sending (`Ctrl+Alt+C`) did the same. The crop and
the desktop's picture did not, because each turned the picture itself on the
way: the rule lived in its callers, and two of four had it.

`nitid convert` writes through the same path, and a batch of phone pictures is
exactly the input that shows both faults on every file.

## Decision

**The export writes the profile that says what its numbers mean.** Kept, the
picture is written with the file's profile encoded back out of the parsed one
(`ColorProfile::encode`). Baked, nothing is written: sRGB is what a file says
by saying nothing. An untagged picture stays untagged (ADR 0005).

Two kept pictures are baked instead, so the colour arrives right rather than
under the wrong name:

- **HDR.** A PQ signal in an eight-bit standard-range file is not a picture any
  reader shows as one. The box already said "HDR to SDR, as on an ordinary
  screen" for both choices; now both choices do it.
- **A profile the writer refuses** — a curve the ICC format has no type for.
  Rare, and said in the box before anything is written.

**The export turns the picture the way it is shown.** The request carries the
orientation, file tag and turns composed, and the export applies it after the
colour and before encoding, on the eight-bit copy. One place, so a fifth
caller cannot be the one that forgets.

**The request can carry an EXIF block**, for `nitid convert`, which writes the
same photograph in another container and keeps its account of itself. A save
from the window still writes none: it is a new picture of what was on screen,
and ADR 0028 relies on it producing a clean file.

## Consequences

- A kept profile, the HDR bake, the unwritable profile, all eight
  orientations and the EXIF block are each held by a test that a mutation of
  the rule fails.
- The original bytes of a profile are not what is written: the profile is
  parsed on open and encoded again on save. For the matrix-and-curve profiles
  photographs carry, the conversion it describes comes back the same (the test
  compares the matrices); a profile with tags the parser does not keep loses
  them. Carrying the raw bytes through decoding would mean a second field on
  every decoded picture for a difference no reader has shown.
- Turning costs a pass over the pixels on a turned picture, and nothing on an
  upright one.
