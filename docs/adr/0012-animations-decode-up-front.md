# 12. Animations decode up front and play on the event loop's clock

Date: 2026-08-23

## Status

Accepted.

## Context

GIF, APNG and animated WebP opened as their first frame, which reads as a
bug: the file plainly moves everywhere else. Playing them raises three
questions — when frames are decoded, who owns the clock, and what happens to
the promise that a still viewer costs no GPU time and no wakeups.

## Decision

**Every frame is decoded and composited up front**, at open time, into
full-canvas RGBA. The decoders handle disposal and blending between frames,
so playback is nothing but swapping pixels on a clock — allocation-free, and
an animated neighbour prefetches like any other image. The price is memory,
and it is capped at 256 MB of frames per animation; past that the file shows
as its first frame, which is what every earlier version did for all of them.
The alternative — decoding frames on the fly — keeps memory flat but puts a
decoder on the playback path, where a slow frame becomes a stutter.

**The clock lives in a `Player` owned by the shown image**, plain arithmetic
over `Instant`s with no window or GPU in reach, which is what makes the
timing testable. The event loop drives it from `about_to_wait`: a playing
animation asks to be woken exactly when its next frame is due
(`ControlFlow::WaitUntil`), a paused one or a still asks for `Wait` — the
event-driven promise stands, and pausing restores the silence completely. A
frame tick writes pixels into the texture already on screen rather than
rebuilding texture and bind group; the frames of one file share a size and a
profile, so everything else stands.

**Conventions over the files' letter, in two places.** Frame delays of 10 ms
and under play as 100 ms — the convention every browser applies, and the one
zero-delay files are written against. Loop counts are ignored: a viewer keeps
playing, and the space bar is how a frame is held still. On an animated image
the space bar is its pause; on a still it steps to the next file, as it
always has.

**A clock that falls behind rebases rather than replays.** After a sleep or
a stall the animation continues from where it is; fast-forwarding through an
hour's backlog frame by frame serves nobody.

## Consequences

The frame counter lives in the title bar, which is the status line this
stage of the interface has.

One decoder flaw surfaced and is recorded where it is tolerated: `image-webp`
blends an animation frame onto its canvas with a scale of 2^24/255 rounded
down, so a fully opaque blended frame loses one part in 255 — a 255 channel
comes back as 254. Pillow reads the same file exactly. Invisible in practice;
the fixture test allows the single unit and names this cause, and the hub
watches the upstream.
