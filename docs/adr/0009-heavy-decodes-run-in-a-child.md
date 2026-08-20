# 9. Heavy decodes run in a child process, so they can be stopped

Date: 2026-08-20

## Status

Accepted. Gives the boundary from
[ADR 0002](0002-sandbox-c-decoders.md) its first occupant, for a reason that
ADR did not anticipate.

## Context

ADR 0002 built a separate process for decoders that were memory-unsafe. By
v0.8.0 that reason had evaporated: HEIC decodes in Rust (ADR 0007) and AVIF
decodes through `rav1d` (ADR 0008), so nothing needed confining and the
machinery sat unused for three releases.

A different reason turned up in its place. A decode running on a worker thread
in this process **cannot be stopped**. Rust has no way to interrupt a thread,
and the decoders are third-party crates with no cancellation of their own. So:

- Navigating away from a large image does not stop decoding it. The reply is
  discarded on arrival, which is correct, but the work is done first — and on a
  folder of 12-megapixel HEICs a held arrow key falls a full decode behind per
  press.
- A decoder that wedges on a crafted file wedges a worker with it, for ever.
  There are two workers; two such files leave the viewer unable to decode
  anything at all.

A child process fixes both, because a process can be killed.

## Decision

`Format::needs_sandbox()` answers true for HEIC and AVIF — the two decoders
large and slow enough that a crafted file could keep one busy for a long time.
The cheap decoders stay in-process, where the round trip would cost more than
the decode.

Cancellation is separate and cheaper, and applies to every format: a job is
abandoned if the user has navigated past it, checked between reading the file
and decoding it. On a folder of large images the read alone is long enough for
a held arrow key to move on more than once.

The reply carries **what the decoder learned, not only its pixels** — the
orientation and the ICC profile travel with them, and the protocol version is
2. Reading those in the parent instead, as v0.6.0 did, works for a format that
states them in its container and silently loses them for AVIF, where the colour
description lives in the AV1 bitstream and only the decoder ever sees it.

The timeout that shipped in v0.6.0 is now **exercised**. It was untested code
for three releases: there was no way to make a decoder hang on purpose and no
way to shorten thirty seconds. Both are now possible through the environment,
and `tests/sandbox.rs` holds a real wedged child to the timeout — verified by
removing the timeout and watching the test hang, which is what a viewer would
have done.

## Consequences

The cost was measured rather than estimated. Starting the child is close to
free: a small HEIC opens in the same time it did before. Carrying the pixels
back is not — a 12-megapixel HEIC went from about 830 ms to 1060–1220 ms,
which is 48 MB crossing a pipe. The owner's decision was to keep the boundary
and make the crossing cheaper later, with shared memory instead of a pipe; that
has its own entry in the plan.

What this does **not** do is close the network to the decoder. That gap has
been recorded since v0.6.0 and is now measured rather than suspected: a decoder
taught to try reported `listen=true connect=true` from inside the boundary. A
low integrity token does not close a socket, whatever the common belief.

Closing it needs an AppContainer with no capabilities. That was attempted here
and turned out to be a stage of its own rather than a step in this one: the
container's SID must be granted access not merely to `nitid.exe` but along
every directory leading to it, which means the viewer editing permissions on
folders it does not own. The attempt was reverted rather than left
half-finished in the tree, and the finding is written into the plan so the next
attempt starts from it.
