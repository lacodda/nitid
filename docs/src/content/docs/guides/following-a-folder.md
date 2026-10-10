---
title: Following a folder
description: The open folder is followed as it changes; N puts up the newest picture as it arrives, Ctrl+F narrows the walk to the names that match.
---

A folder is not finished just because it is open. A generator keeps writing
into it, a camera tethered to the laptop keeps dropping frames into it, an
editor saves over the picture you are looking at. nitid keeps up with all of
that by itself, and with one key it shows each new picture the moment it lands.

## The folder is followed while it is open

The listing is not a snapshot taken when the file opened. While a folder is
open, Windows tells nitid every time something in it changes, and nitid reads
the folder again once it has gone quiet for a quarter of a second — a picture
written in a hundred pieces is read once, not a hundred times.

- **A picture written into the folder joins the walk** where the folder's
  order puts it, and a message names it. The count in the status line grows;
  the picture on screen stays where it is.
- **A picture taken away leaves the walk.** If it was the one on screen, the
  next one comes up — the same as `Del` in the viewer, forwards rather than
  back — and a message says it is gone. A folder emptied completely keeps the
  last picture on screen and says so; when something arrives in it again, that
  comes up.
- **The picture on screen written again is loaded afresh**, framed where you
  were looking at it: an edit checked at 400% is seen at 400%. Save from the
  editor `E` opened, look back, and the edit is there.
- **The picture on screen renamed in Explorer stays on screen** under its new
  name: a file that went and a file with exactly its size and time that came in
  the same moment are one picture, not two.
- **A file still being written is left alone** until the program writing it
  lets go. Taking it half-way would decode half a picture and show a broken
  file.

What nitid writes itself — a mark, a saved turn, a crop, a clean copy, a picture
saved as another format — joins the listing at once and is not announced a
second time as somebody else's arrival.

The order of what is already listed never changes under you. A file whose date
moves is not re-sorted mid-session, so the next arrow key goes where you expect;
the order is settled when the folder opens, and an arrival is put where that
order would have put it — first, in date order.

A selection of files — several picked in Explorer and opened together — is a
list rather than a place, and is not followed. Neither is a folder Windows will
not report on, such as some network shares; it is walked as it was when it
opened.

## `N` follows the newest

**`N` puts the newest picture up now, and every new one as it arrives.** The
status line says `following` for as long as it is on, and `N` again stops it.
Newest means most recently written, whatever order the folder is shown in — a
folder sorted by name answers it from wherever the newest file sits.

This is the key for the work where the picture is made somewhere else and
looked at here: render, generate, shoot tethered, export from an editor —
and see the result without touching the viewer.

Following is a mode you turn on, not a rule the viewer applies by itself. A
viewer that jumped whenever the newest picture had been on screen would also
jump on its own crop and on a mark written into the newest file, and a picture
that moves by a rule nobody stated is one you end up fighting. `Esc` does not
end it, any more than it lets go of the held framing (`L`): it is a way of
looking, not a box in front of the picture.

It works with everything else:

- **Comparing** (`V`): the pinned picture stays, and each new one comes up
  beside it — pin the best so far and watch the next attempts arrive against
  it.
- **Slideshow** (`S`): a new picture interrupts the show and goes up at once.
- **A box or a crop** in front of the picture holds an arrival back; it comes
  up as soon as the box closes, and a message says it is waiting.

## `Ctrl+F` walks only the names that match

**`Ctrl+F` opens a box for a pattern**, and the arrow keys then walk only the
pictures whose names it admits. The patterns are the ones a file dialog takes:

| Pattern | Walks |
| --- | --- |
| `*.png` | every PNG |
| `IMG_????.jpg` | `IMG_0042.jpg`, not `IMG_42.jpg` — `?` is exactly one character |
| `*.jpg;*.png` | either: several patterns are separated by `;` |
| `final` | any name with `final` in it — a word without `*` or `?` matches anywhere |

Case does not matter, and only the name is matched, never the folders above it.
An empty box walks every name again. The pattern shows beside the count in the
status line, the way `marked only` does for `M`, and the two work together:
`*.png` with `M` walks the marked PNGs.

With the folder followed, the newest is the newest of what the pattern admits —
which is what makes the two worth having together. A generator that writes a
preview JPEG and a full PNG for every picture, a camera that writes a RAW beside
each JPEG: `*.png` and `N`, and only the pictures you want come up.

Like `M`, switching a pattern on does not move the picture on screen even when
the pattern leaves it out; the next arrow key goes into the narrower walk. A
slideshow started with a pattern up shows what the pattern admits, and the
desktop's picture changed from the folder with `Shift+W` takes the pattern with
it — see [The desktop's picture](/nitid/guides/the-desktop-picture/).
