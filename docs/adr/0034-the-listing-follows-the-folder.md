# 34. The listing follows the folder, and following the newest is a mode

Date: 2026-10-10

## Status

Accepted.

## Context

Until v0.37 the folder was read once, when a file opened, and never again. A
picture written into it later was invisible until the folder was opened
afresh; one deleted from Explorer stayed in the walk and failed to open; an
editor saving over the picture on screen left the old pixels up. For the work
the viewer is increasingly used for — a generator writing into a folder, a
camera tethered to the laptop, an export from an editor — the viewer had to be
told every time that the folder had moved on.

Two promises constrain the answer. A still picture costs no wake-ups (the event
loop sleeps until something happens), and the order of the walk does not change
under the person walking it.

## Decision

**The listing is always live, with no setting.** While a folder is open, a
thread waits on `FindFirstChangeNotificationW` for it and says only that
something changed. The event loop waits for the folder to be quiet for a
quarter of a second — but no more than two seconds from the first change of a
run — and reads it again, and the listing is brought into line with what it
holds. The notification is a doorbell, not a list: what changed is worked out
by comparing the folder with the listing, by path and by a stamp of size and
modification time, so there is one account of the folder rather than a second
one kept in step with a stream of events.

What the comparison answers:

- an arrival is put where the folder's order would have put it; what is listed
  already is never re-sorted;
- a file that went leaves the walk; if it was on screen, the next one comes up,
  as a delete in the viewer does;
- the picture on screen written again is loaded afresh, keeping its framing;
- the picture on screen gone with exactly one arrival of exactly its stamp is
  the same picture renamed, and stays on screen;
- a file still held open for writing — asked by opening it while refusing to
  share it with a writer — or still empty is left out until it is finished.

The viewer's own writes — a mark, a saved turn, a crop, a clean copy, a save as
another format — update the listing directly, so reading the folder afterwards
does not mistake them for someone else's.

**Following the newest is a mode, on `N`.** Turned on, it puts the newest
picture the walk admits up at once, and each one that arrives after; the status
line says `following`. The alternative — jump whenever the newest was on screen
— would also jump on the viewer's own crop and on a mark written into the
newest file. A picture that moves by an unstated rule is one the person fights;
a named mode is one they use.

**A name pattern narrows the walk, on `Ctrl+F`.** `*` and `?` as a file dialog
reads them, `;` between alternatives, a bare word matching anywhere in the
name. It narrows the same list the marks filter narrows — both answer "what am
I browsing", and two lists would be two answers — so the two compose, and the
newest is the newest of what the pattern admits.

## Consequences

- A still picture in a quiet folder still costs nothing: the watch thread
  sleeps in the kernel, and the loop wakes for a reading only after a change.
- The watch starts after the first frame, not before: it costs a thread and a
  handle, and nothing that is not the picture goes in front of the picture. The
  folder is read once when it starts, for anything that arrived in between.
- A folder Windows will not report on — some network shares — is walked as it
  was when it opened. A selection of files is a list, not a place, and is not
  followed.
- `Folder::current` is an `Option`: a folder emptied from outside is an
  ordinary state now, and it filled again shows what arrived. Before, `Del` on
  the last picture of a folder panicked asking an empty listing for its name.
- The bookkeeping is a plain function over two listings (`Folder::sync`) with
  tests of its own; a live gate (`tests/live_follow.rs`) drives a real window on
  a real folder, because a deleted line in the event loop would leave every unit
  test green.
