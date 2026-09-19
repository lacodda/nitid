---
title: Sending a picture out
description: Saving a copy without the camera, the date and the place.
---

A photograph carries more than the photograph. EXIF names the camera and the
lens, the date and often the exact place. XMP carries whatever the editing
program felt like writing, which sometimes includes the owner's name. IPTC
carries a caption and a copyright. Send the file to somebody and you send all of
it, and most people do not know that they have.

**`Ctrl+M` saves a copy with that taken out**, beside the original, as
`photo-clean.jpg`.

## Nothing is re-compressed

This is not "save as", and the difference is the whole point. The box says what
the file is carrying, in the same words for every format:

```
Carries: EXIF, XMP (4.2 kB)
```

and then removes exactly that. The compressed picture is **not read, not decoded
and not re-encoded** — the file is walked at the level of its container and the
parts that describe rather than depict are left out of the copy. A scrubbed JPEG
is identical to the original from its start-of-scan marker onward, byte for byte.

That matters because the alternative trades one loss for another. A viewer that
stripped your EXIF by re-encoding the photograph would have removed something you
could not see and damaged something you can.

Works for **JPEG, PNG and WebP**. HEIC and AVIF keep their metadata in a box tree
whose offsets point into the picture data, so removing a box there means
rewriting those offsets — a larger job whose failure mode is a file that no
longer opens. The viewer says so rather than offering something that will refuse.

## The colour profile stays

By default, and deliberately. A colour profile says nothing about you, the
camera or the place — it says what the numbers in the file mean. Remove it and
the picture does not become anonymous, it becomes **wrong**: the numbers now
claim to be sRGB when they are not, and every program that reads them will be
wrong in the same direction.

*Take the colour profile too* is there for a picture whose colour has already
been baked into its numbers with `Ctrl+Shift+S`, or one going somewhere that
ignores profiles anyway.

## The orientation, which is the interesting one

The orientation tag is the one field that is both metadata and load-bearing.
Strip it from a photograph taken with the phone held sideways and the picture is
shown sideways by everything, for ever.

So the box offers to **turn the pixels first**, which is what makes the tag safe
to remove:

```
[x] Turn the pixels a quarter clockwise, so the orientation can go
```

For a JPEG this is done the same way a crop is (see
[Honest color](/nitid/concepts/honest-color/) for the general principle): by
moving the compression's own coefficients. The picture is never decoded. A
transpose of a block of pixels is a transpose of its coefficients, and a mirror
is a sign change on half of them — so the values in the file are rearranged, and
not one of them is recomputed. Turning a file four times gives back the original,
byte for byte.

The price, stated rather than hidden: the picture's dimensions have to be a
multiple of the compression's grid — 16 pixels for the 4:2:0 that phones and
cameras write. A JPEG's last block in a row is partial, and the pixels past the
edge are padding the decoder throws away; that is harmless while the edge stays
an edge, but a turn would bring the padding into the middle of the picture. When
that would happen the viewer **keeps the orientation tag instead** and says so,
rather than producing a seam nobody asked for:

```
photo-clean.jpg — the turn was kept as a tag: the edges do not fall on the
compression grid
```

Everything else is still removed. A picture that keeps its orientation has kept
one number about which way up it is, and lost the camera, the date and the place.

## What is left, and what is not

| | Removed | Kept |
| --- | --- | --- |
| **JPEG** | EXIF, XMP, IPTC, comments | the colour profile, the picture |
| **PNG** | `eXIf`, XMP, text chunks, `tIME` | `iCCP`, the picture |
| **WebP** | `EXIF`, `XMP ` | `ICCP`, the picture |

An `APP` segment whose signature the viewer does not recognise is **left where it
was**. Dropping every unknown segment would mean removing parts of files nobody
here can describe, and a maker note this viewer cannot read is not automatically
something you want gone.

Running it twice changes nothing the second time, so a copy of a copy is the same
file.
