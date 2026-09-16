# 27. A crop moves coefficients, not pixels

Date: 2026-09-16

## Status

Accepted, v0.31.0. Revisits the question ADR 0024 answered for turning.

## Context

Cropping a picture is decoding it, taking a rectangle of pixels and encoding
those again. For PNG or WebP that costs nothing — both are lossless, so the
second file holds exactly what the first did.

For JPEG it is not free. Every re-encode quantises again: ringing appears at
edges, blocks appear in flat sky, and a photograph that has been through the
process a few times shows it. A viewer whose whole promise is an honest picture
should not be the program that degrades one.

ADR 0024 met the same wall for turning and went round it: the orientation is a
tag, so a turn is written as a label and the pixels are never touched. A crop
has no tag. There is nowhere to write "show only this part" that every other
program would honour, so the pixels — or something below them — have to move.

## Decision

**Crop at the level of the coefficients, in our own pure-Rust code.**

A baseline JPEG holds the picture as 8×8 blocks of quantised discrete-cosine
coefficients, grouped into minimum coded units and then entropy-coded with
Huffman tables. Only the last of those three steps is undone: the Huffman
stream is decoded to coefficients, the ones inside the rectangle are kept, and
they are coded again. The quantisation tables are copied across untouched and
no coefficient is ever dequantised, so nothing is turned into a pixel and back.

The bytes of the new file differ, because the Huffman coding is redone over a
smaller image. The picture does not: every surviving coefficient is the number
the original file held, measured in the same quantisation table. A test asserts
this on the decoded samples against the original's own, demanding exact
equality rather than similarity, and a second test compares the quantisation
tables — which a pixel comparison cannot see, since a careful re-encode can
produce similar pixels from rebuilt tables.

### What "the same picture" means at the cut edge

For a 4:4:4 file, the decoded pixels are identical to the original's, to the
byte. For a **subsampled** file — 4:2:0, which is what cameras write — the
interior is identical to the byte and a border one pixel wide is not.

Measured rather than reasoned: on the committed 4:2:0 fixture an offset crop
differs in 412 pixels of 10240, every one of them on a cut edge, none of them
interior, by at most 16 of 255. The chroma plane is half resolution, so the
decoder interpolates it, and a pixel on the cut edge was interpolated in the
original against a neighbour that is now outside the picture — so the decoder
uses the edge sample instead. The coefficients it interpolates from are the
same ones, and no second quantisation has happened.

That is the honest shape of the promise: the file's own coefficients and
tables, not a guarantee about the last row of samples at a boundary that did
not exist before. The gate holds the interior exactly and bounds the border,
rather than loosening the whole comparison to accommodate an edge.

### Why not `turbojpeg`

`jpegtran` does this, and binding it would have been a day's work rather than a
week's. It was rejected for the reason ADR 0024 rejected it and ADR 0002 and
ADR 0007 rejected the C decoders: every decoder here is pure Rust on purpose,
and the C paths cost vcpkg, meson or libclang to build at all. Taking the
dependency now would overturn a decision made six versions ago for one feature.

There is no pure-Rust crate that does it — searched, not assumed — so the
choice was to write it or to abandon the promise.

### Why not abandon the promise

"Decode and re-encode" is the thing the stage exists to avoid. That path
already exists — it is `export`, from v0.30.0 — and it remains the fallback.
Shipping only it would have meant a crop feature whose headline is a defect.

### What this buys beyond the stage

The tree now has a coefficient-level JPEG module it did not have. v0.32.0 is
metadata stripping, which promises to bake the orientation into the pixels
without loss; that is the same machinery. The class of "we would need
`jpegtran` for this" is closed rather than deferred.

## The grid, and saying so

The rectangle has to fall on MCU boundaries — 16×16 pixels for the 4:2:0 that
most cameras write, so an arbitrary crop is up to fifteen pixels from a usable
one. The near edges must be on the grid; a far edge may be the picture's own
edge, because the file's last MCU is partial there already, and refusing that
would mean the right-hand side of most photographs could never be cropped to
losslessly.

**The bar says which crop is about to happen, before it happens.** It names the
size the edges would move to, or says the box is already on the grid, or says
the file has to be re-encoded. It never silently does one having said the
other, which is the same rule the save box follows about what an export costs.

Snapping shrinks towards the inside of what was asked for. Someone dragging a
crop box has framed something; giving them back pixels they deliberately
excluded is a worse surprise than losing a few they included.

## What is refused

Progressive JPEGs, twelve-bit samples, non-interleaved scans and arithmetic
coding go to the fallback, and the refusal is named rather than silent — a
`Refusal` value the interface can report. "It re-encoded and I do not know why"
is exactly the silent-default class of defect this project treats as a bug.

Progressive is the one worth naming: its coefficients are spread across several
scans with successive approximation, and rebuilding that for a smaller image is
a much larger job than this. It may be worth doing later; it is not worth
holding the stage for.

## A crop is a copy

It lands beside the original under a free name, never in place. Cropping
discards pixels, and a viewer that did that to the file itself could destroy a
photograph with one keystroke and no undo. This is the opposite of ADR 0024's
case — a turn changes a label and can be turned back — which is why the two are
different keys.

The re-encoding fallback writes **PNG** rather than JPEG. A crop that has to be
re-encoded should at least not lose anything a second time.

## Orientation

A crop is framed in the coordinates the picture is *seen* in; a file's grid is
in the coordinates it is *stored* in. Rather than mapping one onto the other —
a second place for the orientation rule of ADR 0018 to live, and so to drift —
the lossless path is offered only where the two agree, and a turned picture
takes the re-encoding path. The rectangle for that path is read through
`eyedropper::sample`, which is the one place in the viewer that turns a shown
pixel into a stored one.

## Consequences

The viewer carries a JPEG entropy coder, in both directions, that it did not
have. It is about four hundred lines and is exercised by twelve unit tests and
seven live ones, three of which were checked by mutation: writing the DC
coefficient absolutely rather than as a difference, ignoring the rectangle's
origin, and writing one block per component per MCU. All three fell a gate,
which is what says those gates are gates rather than tautologies.

The third of those is the reason `tests/fixtures/subsampled-420.jpg` is
committed — the first binary fixture in this repository, where everything else
is built in code. The `image` crate encodes **4:4:4 only**, so every fixture
built here has one block per component per MCU, and the mutation that writes
exactly that survived all 572 unit tests and every integration suite. A whole
branch of the coder — the one the 16x16 grid exists for — was untested while
the suite was green. The fixture is synthetic and encoded by libjpeg through
Pillow, so it is both a real 4:2:0 file and a third-party encoding.

The DC coefficient is why a crop is not a copy of bytes. JPEG stores it as a
difference from the previous block of the same component, so the first kept
block of each component refers to a block that is no longer in the file. That
alone forces a decode to coefficients and a re-code from new differences — and
it is the one thing here that would be silently half-wrong if got wrong, since
a picture with the wrong DC values still decodes.
