---
title: Working through a folder
description: Culling, deleting, renaming and sorting files through the shell's own operations.
---

A viewer that can only look is half a tool. Going through a shoot means
throwing some frames away, naming the keepers, and putting them where they
belong — and doing that in a file manager means leaving the picture to look at
a list of names, which is the one view that cannot answer "is this the good
one".

**`Del` sends the file to the recycle bin** and moves to the next picture.
Forwards, not back: culling a folder carries on the way you were going, and
landing on the frame you just judged would mean judging it twice. Nothing is
asked first — the bin is undoable, and Explorer's own `Ctrl+Z` takes the
operation back — because a confirmation on every frame is what makes people
stop culling in a viewer and go back to a file manager.

**`P` keeps this one, `X` rejects it, `U` takes the mark back off.** The
letters are Lightroom's, because culling is the one job in a viewer where a key
is pressed hundreds of times in a row and the hands that have done it before
already know where to go. Pressing the mark a picture already carries takes it
off, so the way back is the key you just pressed rather than a second one to
remember.

**A reject is a mark, not a delete.** The point of a pass is to get through the
folder without stopping to decide anything irreversible, and then look at what
came out. `Del` is still there for the frames you are sure about.

**The mark goes into the file, where everything else can see it.** It is
written as the EXIF rating Windows itself uses, so a picture you kept shows up
with a star in Explorer's own Rating column, in its Properties, and in any
program that reads a rating — this is the whole reason a viewer's selection is
worth making. The alternative, a small file written beside the picture, would
have been a mark only nitid could see, and it would be left behind the first
time the photograph was copied somewhere.

Only the metadata block is rewritten. A marked JPEG is identical to the one
that went in from its start-of-scan marker onward — the same promise `Ctrl+S`
makes for a turn, and for the same reason: nothing here runs an encoder over
your pixels. A format with nowhere to put a rating says so rather than
pretending the mark was taken.

**`M` walks only the pictures that are marked.** The arrow keys then step
between the frames you judged and skip everything you did not, and the count in
the status line says `marked only` so the mode is never something you have to
deduce. Switching it on does not move the picture you are looking at, even when
that picture is one the filter leaves out: the frame you just judged is usually
still on screen, and it is the next arrow key that takes you into the
selection. `M` again gives the whole folder back. With nothing marked yet, the
viewer says so instead of leaving the arrow keys quietly doing nothing.

**`F2` renames it.** The box opens with the whole name in it and the stem
selected, so typing replaces the name and leaves `.jpg` alone, while renaming
`shot.jpg` to `shot.png` is still one keystroke away. Enter commits, Escape
cancels, and the box holds the keyboard while it is open — typing "gull" must
not also step through the folder and turn the picture. A name carrying a path
separator is refused rather than quietly moving the file somewhere else.

**`Ctrl+1` through `Ctrl+9` sort it into folders you set**, and
`Ctrl+Shift+1`-`9` copy it there instead of moving it. The folders are set in
the Files section of the settings; a key with nothing set says so rather than
doing something surprising. The digits carry `Ctrl` because bare `0` and `1`
have fitted the picture to the window and shown it at 100% since v0.1.0, and a
sorting key added later does not get to take a viewing gesture that has been
there from the beginning.

**`E` opens the picture in the program that edits it.** Not a copy, not an
export — the file on screen, in whatever Windows already associates with that
kind of image, which is the same program the context menu's own "Edit" would
start. It works without being configured; naming an editor in the settings
only overrides the choice. `Alt+1` through `Alt+9` open the file in programs
you name yourself, for the ones the system would never pick: a raw converter,
a batch stamper, an upload script.

Nothing waits for the program to close, and the picture is not reloaded when
it does — an edit takes as long as it takes, and `R` reloads when you want it.

**`Ctrl+S` keeps a turn.** `R` and `Shift+R` turn the picture on screen and
always have; pressing `Ctrl+S` writes that turn into the file, so it stays
turned in every other program too. Nothing is re-encoded to do it — the file's
orientation tag is rewritten and the compressed image data is left exactly as
it was, byte for byte. A photograph turned this way a hundred times is the same
photograph. `F` mirrors the picture left to right, `Shift+F` top to bottom, and
those save the same way.

**`Ctrl+Shift+S` saves the picture as something else.** JPEG, PNG or WebP,
beside the file it came from, under a name you type — the original is never
touched and an existing neighbour is never replaced.

What comes out is what was on screen. The colour path is the one the shader
draws with, step for step: the same curves, the same matrix, the same clamp,
run over every pixel instead of the visible ones, and a gate reads the shader
itself to keep the two from drifting apart. So a wide-gamut photograph can go
out with its colour **baked into sRGB** — the numbers say what you were
looking at, which is what a chat window or a forum will show, since neither
reads a profile. Left unbaked, the file keeps its own numbers and its profile
travels with them; that is the default for an ordinary picture, because it is
the choice that can be undone.

The box says what a save will cost before it writes anything. JPEG has no
transparency, so a picture with any will be filled with white. Eight bits per
channel is all JPEG and WebP store, so a sixteen-bit source loses the rest. And
an HDR picture becomes SDR the way it already looks on an ordinary screen:
reference white lands on white, and highlights above it clip. That is a real
loss and it is stated plainly — those highlights are not coming back — but it
is exactly what you were seeing, rather than a second rendering that would make
the file disagree with its own preview.

**`Ctrl+X` frames a crop.** The box opens over the whole picture; drag a corner or
an edge to bring it in, drag inside it to slide it about, or press somewhere
clear of it to draw a new one. The buttons along the bottom hold it to a shape
— 1:1, 3:2, 4:3, 16:9 and their upright forms, or the picture's own — and the
box reshapes as soon as you pick one rather than waiting for the next drag.
`Enter` takes the crop, `Esc` leaves without taking it. The handles stay the
same size under the pointer whatever the zoom, and the box is remembered in the
picture's coordinates, so zooming or panning mid-crop moves the view and not
the framing.

**A crop is always a copy.** It lands beside the original as `photo-crop.jpg`,
and a second one as `photo-crop-2.jpg`; the file you were looking at is never
written to. That is the difference between this and `Ctrl+S`: a turn is a label
and can be turned back, while a crop throws pixels away, and a viewer that did
that in place could destroy a photograph with one keystroke and no undo.

**A JPEG is cropped without being re-encoded, where it can be.** JPEG stores a
picture as blocks, and a crop whose edges land on those blocks can be made by
moving the compressed data itself — no decoding, no quantising, no encoder in
the path at all. The surviving coefficients are the numbers the original file
held and its quantisation tables travel with them, so the kept part of the
picture is bit-for-bit what it was. Crop a photograph fifty times this way and
the fiftieth is as clean as the first.

One honest detail, since the point of the feature is honesty: on a subsampled
JPEG — which most photographs are — a border one pixel wide along the cut can
shift very slightly. The colour channels are stored at half resolution and the
decoder interpolates them, so a pixel on the new edge no longer has the
neighbour it was interpolated against. Nothing is re-quantised and the interior
is identical to the byte; it is the boundary that had to be invented, and it is
invented once rather than accumulating with every crop.

The blocks are usually 16×16 pixels, so an arbitrary crop is up to fifteen
pixels from one that can be done this way. The bar says which you are about to
get: it names the size the edges would move to, or says the crop is already on
the grid, or says the file has to be re-encoded and lets you decide. It never
quietly does one when it said the other. A progressive JPEG, a PNG, a HEIC —
anything that cannot take the coefficient path — is decoded, cut and written as
a **PNG**, which is lossless, so a crop that must be re-encoded at least does
not lose anything twice. A sixteen-bit source goes out at eight bits per
channel on that path, and the bar says so before it happens.

Turning and saving are separate on purpose: looking at a photograph from
another angle leaves nothing on disk until you say so. The one thing to know is
that a program which ignores EXIF orientation — a few old tools do — will still
show the original; the trade is deliberate, and the reasoning is in ADR 0024.

Everything here goes through **the shell's own file operations**, never a
direct write. That is what makes a delete land in the recycle bin instead of
being gone, puts the operation on the shell's undo stack, and resolves a name
that is already taken the way the rest of Windows resolves it — a second frame
of the same name lands beside the first rather than replacing it. A viewer
that deleted with a plain filesystem call would be a viewer that loses
photographs, whatever it said in a confirmation dialog.
