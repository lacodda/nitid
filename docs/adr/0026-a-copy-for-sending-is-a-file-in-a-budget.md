# 0026. A copy for sending is a file in a budget, not a bitmap

## Status

Accepted, v0.30.0.

## Context

`Ctrl+C` puts the picture on the clipboard as a `CF_DIB` — the file's own
pixels, uncompressed, with no profile (ADR 0019). That is the right answer for
an editor that paints, and the wrong one for sending a photograph to somebody:
a mail client or a chat window receiving a bitmap compresses it however it
likes, at whatever quality it likes.

So "copy this small enough to send" cannot be done by a bitmap at all. Asking
for a copy under 500 KB only means something if what travels is a file.

## Decision

**Two formats go on the clipboard at once.** `CF_HDROP` naming a JPEG, and
`CF_DIB` with the pixels — the same offer a drag already makes (ADR 0021). The
receiving application takes the one it understands, which is how the shell's
own copies work.

**The JPEG is made to a budget in kilobytes, not to a quality.** Quality is
searched for: what a given setting costs depends entirely on the picture, so
the encoder is asked rather than guessed at, by a binary search over the range
— seven encodes at most. The best-looking file that fits is the one that goes.
A budget nothing can meet is reported rather than quietly exceeded.

The picture is shrunk to a maximum width first, by averaging rather than by
picking nearest pixels, and its colour is baked into sRGB: what travels is a
file going to a program that will very likely ignore a profile.

Both numbers are settings — 500 KB and 2048 pixels by default, which is under
every ordinary attachment limit and still a picture rather than a thumbnail.

## Writing to disk

A `CF_HDROP` names a file, so there has to be one. ADR 0020 says a viewer does
not write to disk unasked — and this is asked: the whole of the gesture is
"give me a file I can attach".

It goes to a folder of its own inside the temporary directory, named for the
process so two viewers copying at once cannot collide, and the file is named
after the picture so what arrives in a chat carries a name that means
something. Nothing appears in the folder the user is looking at.

## The key

`Ctrl+Alt+C`, decided by the owner. Both natural slots on C were taken —
`Ctrl+C` copies the picture, `Ctrl+Shift+C` copies the path — and keeping all
three on the same letter says what they have in common, with the modifier
saying which.

It is the first chord in the viewer with two modifiers, which makes the order
of the dispatch load-bearing: a branch that asked about Alt first would route
`Ctrl+Alt+C` to the program slots. That order is now a function, `route_for`,
rather than a chain of conditions inside the event handler — a test can ask it
the same question the dispatch does, instead of repeating its condition and
passing even when the branch is deleted.

## Consequences

- A picture can go into a mail or a chat at a size that will be accepted,
  without a trip through "save as" and a file manager.
- The clipboard carries a path to a temporary file that outlives the copy. It
  is small, it is in the temporary directory, and Windows clears that; the
  alternative — deleting it after the paste — cannot be done, because nothing
  tells the source when a paste happened.
- The search costs up to seven JPEG encodes of an already-shrunk picture.
  Measured in milliseconds, and paid on a key press rather than on any hot
  path.
