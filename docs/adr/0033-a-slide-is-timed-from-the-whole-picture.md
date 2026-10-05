# 33. A slide is timed from the whole picture, and a shuffle is a deck

Date: 2026-10-05

## Status

Accepted.

## Context

A slideshow promises each picture a set time on screen. Two things make that
promise easy to break without noticing: pictures take different times to
arrive — a prefetched neighbour is instant, a sixty-megapixel file can take a
second — and an animation has a length of its own. A random order has its own
trap: drawing every next picture at random shows some photographs of a
holiday three times before others once, which is the first thing a room
watching it notices.

The viewer's event loop also promises that a still picture costs no wakeups,
and a show is a clock.

## Decision

**The clock starts when the picture is whole on screen.** Not when it was
asked for, and not when the embedded thumbnail stood in for it. Every picture
gets the interval the setting promises, however long it took to decode. A
file that will not open gets its interval too, with the message up, rather
than being skipped at once — a folder of broken files would otherwise race
through them.

**An animation finishes one cycle.** A slide with an animation stays up for
the interval or for one pass through its frames, whichever is longer, up to a
minute.

**Nothing moves under a box.** While a rename, a save, a clean copy or a crop
is open over the picture, the clock stands still and starts again when the box
goes.

**The loop wakes for the next slide and nothing else.** The clock names one
deadline; paused, or waiting for a picture, it names none, and the loop sleeps
as it does on a still picture. A step that goes nowhere — a filter that left
one picture — restarts the count rather than trying again at once.

**A shuffle is a deck.** Each round shows every picture the arrow keys would
walk once, in a fresh order; a new round never opens on the picture the last
one closed on. The left arrow goes back through what was actually shown and
the right arrow retraces it. A picture deleted or filtered out since the deal
is passed over. The next picture of the deck, not the folder's neighbour, is
the one prefetched. The order comes from a SplitMix64 written in a few lines
rather than a crate for one function.

**A show goes round.** It wraps at the ends of the folder whatever the wrap
setting says: that setting is for working through a shoot in order, and a show
for a room goes on.

## Consequences

- The clock, the deck and the interval ladder are plain values with their own
  tests, the way the animation player and the blink are; the event loop only
  asks them when to wake and where to go.
- A slideshow's pace shown in the status line is the setting, and the arrow
  keys write it back, so the next show starts at the pace the last one was
  left at.
- A show started on a picture whose decode is still running waits for it: the
  first slide is the picture on screen, counted from when it is whole.
