<p align="center"><img src="https://github.com/lacodda/nitid/raw/main/assets/banner.svg" alt="nitid - a fast image viewer with honest color" width="720"></p>

# nitid

**A fast image viewer for Windows that shows the picture as it actually is.**

Double-click a file and the image is already on screen — no white flash, no spinner. Colors are the ones the photographer chose, not an approximation. On an HDR display you see what the display was bought for.

`nitid` is Latin for *clear, bright, sharp* — the name describes the difference, not the function.

## Why another viewer

| | Startup | Color management | HDR | Modern formats |
| --- | --- | --- | --- | --- |
| Windows Photos | 1–2 s | partial | partial | yes |
| IrfanView / XnView | fast | formal | no | partial |
| ImageGlass | fast | yes | no | yes |
| qView / JPEGView | fast | no | no | no |
| **nitid** | **target < 100 ms** | **ICC on the GPU** | **yes** | **yes** |

Nothing on the market closes all four columns at once. That gap is the whole reason this exists.

## How it gets fast

Speed here is not a faster decoder — it is a different order of operations:

1. The embedded EXIF thumbnail is decoded in single-digit milliseconds and drawn immediately.
2. The full image decodes on a background thread and replaces it without a flicker.
3. Neighbouring files in the folder are prefetched, so arrow keys never wait.

Measured on a 24-megapixel JPEG, from process start:

```
window created at    11 ms
gpu ready at        118 ms
thumbnail up at     128 ms   <- the picture is on screen here
first pixels in     146 ms
```

The full decode of that same image takes about 120 ms and lands afterwards,
replacing the thumbnail in place. Most of what remains is the graphics driver
starting up, not work nitid controls — which is why the order of operations
matters more than decoder benchmarks.

Run with `NITID_STARTUP_REPORT=1` to get that breakdown for your own machine.
The numbers are held to a threshold by `tests/startup.rs`, so a change that
puts a full decode back on the startup path fails the build rather than
quietly costing a tenth of a second.

## Honest color

An image carries a colour profile saying what its numbers mean; a display has
one saying what it can show. Most viewers ignore both and send the numbers
straight to the screen, which is why the same photo looks oversaturated in one
program and right in another.

nitid reads the profile out of the file, asks Windows what the display is, and
converts between them **in the shader** — the decoded pixels stay as the file
stored them, the conversion costs nothing per frame, and changing your display
profile costs a redraw rather than a reload.

- A wide-gamut file (Display P3, Adobe RGB) is brought into what the display
  can actually show, rather than clipped.
- A file with **no** profile is shown exactly as it is, the way Windows, the
  shell preview and every browser show it. It looks the same here as it does
  everywhere else — including in whatever tool made it. Guessing sRGB and
  converting from it would visibly wash the picture out on a wide-gamut
  display; see [ADR 0005](docs/adr/0005-untagged-images-pass-through.md).
- Arbitrary tone curves are handled by sampling them, so a scanner or camera
  profile costs the same as a simple gamma.

When the image and the display already agree, no conversion happens at all and
the hardware does the sRGB decoding for free.

## HDR

On a display in HDR mode, nitid outputs extended-range linear light — scRGB,
`ExtendedSrgbLinear` on an `Rgba16Float` surface — so highlights above SDR
white drive the display's headroom instead of clipping at white. It is the
same shader either way: the colour transform already ends in linear light, and
an HDR surface simply takes it unencoded and unclamped. An SDR image therefore
looks identical on both surfaces, which is asserted by drawing it twice and
comparing the pixels rather than by eye.

The choice follows the display rather than being made once. Turn HDR on in
Windows with nitid open and the swapchain is reconfigured without a restart;
turn it off and it goes back. Turning it *on* announces itself to the window,
so that direction costs nothing; turning it *off* announces itself to nobody at
all, so while — and only while — nitid is on an HDR surface it asks the display
once a second, for the 140 microseconds that costs. On an SDR display nothing
polls and the event loop sleeps until you act, exactly as before.

`NITID_STARTUP_REPORT=1` states which surface is up and how much headroom the
display reports:

```
nitid: surface Rgba16Float ExtendedSrgbLinear, display headroom 7.71x
```

A screenshot of an HDR window is a standard-range image, so this line is the
one way to check the answer rather than judge it. See
[ADR 0013](docs/adr/0013-hdr-output-goes-through-scrgb.md).

## One window

Double-clicking a second image does not open a second viewer. The launch finds
the one already running, hands its file over and exits, and the picture appears
in the time it takes to decode — measured at 135 to 155 milliseconds against
320 to 560 for a cold start, because the window and the graphics device are
already there.

The same answers multi-select. Windows starts one process per selected file, so
five files means five launches; four of them hand their file to the first, and
the five arrive as one list in one window. Arrow keys then walk that selection
rather than the whole folder — the five you picked, not the hundreds beside
them. Five files opened this way took 239 milliseconds altogether.

The window that owns the channel is simply the first one to create it, which is
a single atomic call, so two launches racing cannot both decide they are the
window. Nothing polls: a hand-over wakes the event loop the same way a finished
decode does, and a still image still costs no wakeups at all. See
[ADR 0016](docs/adr/0016-one-window-elected-by-a-named-pipe.md).

## The interface

The chrome is not there while you are looking at a photograph. There is a
status line along the bottom saying what is on screen — the file, where it sits
in the folder, its size, format, bit depth, what the colour transform is doing,
and the zoom — and everything else appears when you reach for it.

Move the pointer to the top of the window and a toolbar comes down: step
through the folder, zoom, fit, actual size, turn, the zoom lock, the backdrop,
full screen. It carries nothing the keyboard does not, and every button names
its key. Press `?` for the full list.

**It is not on the way to the picture.** Laying the interface out and building
its place on the GPU costs around forty milliseconds, so the first frame is the
photograph alone and the chrome arrives on the frame after — measured at 44 to
86 milliseconds behind it, and held in that order by a test rather than by
intent. The startup promise is unchanged: first pixels in 407 to 509
milliseconds on the same file that took 489 to 528 before the interface
existed.

Drawing it correctly on an HDR surface took a detour worth knowing about. egui
picks how to encode its output from whether the target is an sRGB format, and
the extended-range surface is not one — drawn straight onto it, a mid grey came
out 2.35 times too bright. So egui draws into an sRGB texture of its own and a
shader of nitid's composites that onto the surface, asking the same question
the image shader asks. The interface also stops at SDR white: a toolbar pushed
into the display's headroom would compete with the photograph. See
[ADR 0017](docs/adr/0017-the-interface-is-composited-through-our-own-shader.md).

Nothing here polls. A frame is laid out only when it would look different from
the last one, and the only thing that asks the loop to wake is a message while
it is fading.

## What the file says

Press `I` and a panel comes down the right-hand edge with everything the file
has to say: its size, format, bit depth and colour, its weight on disk and
where it lives, and — for a photograph — what the camera wrote. Make and model,
lens, shutter speed, aperture, ISO, focal length with its 35 mm equivalent, and
when the picture was taken. A photograph carrying GPS gets its coordinates.

It is an overlay, so the picture keeps its framing while the panel is up.

**Every row copies its value when clicked.** A lens name, a shutter speed or a
coordinate is nearly always wanted somewhere else — a caption, a search, a map
— and reading it off the screen to type it back in is the part that wastes the
panel.

Values are shown the way a photographer reads them, not the way the standard
stores them: `1/250 s` rather than 0.004, `f/2.8` rather than 28/10. A maker
that repeats itself in the model is printed once.

The coordinates stay on your machine. nitid does not open a map, or ask any
service where a photograph was taken — a viewer that reached out to place your
pictures would be telling somebody else where you have been.

## Controlling the view

Three things you can do to a picture without touching the file.

**Hold the framing across a step** with `L`. By default every image is framed
for itself, which is what a folder of unrelated pictures wants. Locked, the
arrow keys become a way to compare a series: each frame arrives at the same
magnification over the same part of the picture, so what moves between them is
the only thing that moves. The place is held as a fraction rather than as
pixels, so a neighbour of a different size shows the corresponding part of
itself instead of drifting.

**Turn the picture** with `R`, or the other way with `Shift+R`. It is a viewing
transform: the file is untouched, and stepping to another image shows that one
as its own metadata asks. Rotating the file itself is a later version. The turn
combines with whatever the file already asks for by multiplying their matrices,
in an order that was measured rather than chosen — the two candidate orders
agree on every rotation and differ on every mirror, so a table written by
looking at photographs would be wrong in exactly the cases photographs do not
show. See [ADR 0018](docs/adr/0018-orientation-composes-by-matrix-multiplication.md).

**Choose what shows through transparency** with `B`: the viewer's own dark
scene, a checkerboard, black, or white. Judging a cut-out against one backdrop
is judging it against one background — a logo bound for a white page has to be
seen on white, and a checkerboard is how you tell "transparent" from "a flat
grey that happens to match the scene". The checker is measured in screen
pixels, so it stays the same size at any zoom rather than reading as part of
the picture.

## Large images

A GPU texture has a maximum side — 16384 on current integrated hardware, and
as little as 2048 on the oldest cards nitid still runs on. A stitched panorama
or a scanned map goes past it, and the way that failed is worth knowing: the
graphics API has no way to return the rejection, so it reports it through a
side channel and the default handling is a panic. Before this version such a
file decoded all the way through and then took the viewer down at the moment
it was about to appear.

nitid cuts such an image into tiles the device will hold and draws them as one
picture. Zoom and pan work as they do on any other image, and the joins are
invisible: each tile carries one pixel of its neighbour so the filter has a
real texel to interpolate towards, rather than the repeated edge that leaves a
visible step under magnification. An image that fits in one texture is still
one texture and one draw call — tiling costs it a single comparison.

Still bounded by memory rather than by the texture limit: a tiled image holds
every pixel at once. See
[ADR 0015](docs/adr/0015-large-images-are-tiled.md).

## Formats

Everything below decodes in pure Rust: a malformed file costs an error, never
code execution.

| Format | Extensions | Embedded ICC profile |
| --- | --- | --- |
| JPEG | `.jpg` `.jpeg` `.jpe` `.jfif` | yes |
| PNG | `.png` | yes |
| WebP | `.webp` | yes |
| JPEG XL | `.jxl` | yes |
| HEIC | `.heic` `.heif` `.hif` | yes when embedded; code points converted at decode |
| AVIF | `.avif` | yes, from the bitstream |
| SVG | `.svg` | drawn in sRGB |
| GIF | `.gif` | sRGB by definition |
| BMP | `.bmp` | no |
| TIFF | `.tif` `.tiff` | no |

The format is decided by the bytes, not the extension: a `.png` that is really
a JPEG opens rather than erroring.

GIF, APNG and animated WebP **play**: every frame is decoded up front, the
space bar pauses and resumes, and the title carries the frame counter. Frame
delays of 10 ms and under are read as 100 ms — the convention browsers apply,
which the files were written against. A still image costs no GPU time and no
wakeups; a playing animation wakes the event loop for its next frame and for
nothing else, so pausing restores the silence.

HEIC — the format a modern phone photographs in — decodes in Rust like the
rest, container and HEVC alike, and reaches the screen as fast as a JPEG: it
carries a thumbnail as a second image inside its container, and nitid shows
that first while the full picture decodes behind it. A 10- or 12-bit HEIC
keeps its depth: the decoder hands over sixteen-bit samples and they reach the
texture that wide.

One limitation remains, and only for some files. A HEIC states its colour
either as a set of standard code points or as an embedded ICC profile. With a
profile, nitid reads it and applies it on the GPU like every other format.
With code points — the more common case — the decoder resolves the colour
itself before nitid sees the pixels, so a photograph tagged Display P3 is
shown inside sRGB rather than across a wide-gamut display's full range. See
[ADR 0007](docs/adr/0007-heic-decodes-in-rust.md).

AVIF decodes through `rav1d` — dav1d translated to Rust by the ISRG — with the
container read separately. It gets the full colour treatment: the file's
primaries and transfer curve are read from the bitstream and applied on the GPU
like every other tagged format, so a Display P3 AVIF is shown across the
display's gamut rather than folded into sRGB. 10- and 12-bit AVIF decode at
their own depth: the samples cross the whole pipeline — decoder, sandbox,
texture — sixteen bits wide, never narrowed to eight. An HDR10 file (BT.2020
with the PQ transfer) shows at a sensible brightness on both kinds of display:
its reference white lands on SDR white, and on an HDR display the highlights
above it drive the panel's headroom. See
[ADR 0008](docs/adr/0008-avif-decodes-with-rav1d.md) and
[ADR 0014](docs/adr/0014-pq-reference-white-lands-on-sdr-white.md).

HEIC and AVIF decode in a **separate process** — not because they are unsafe
any more, but because a process can be stopped and a thread cannot. Navigate
away from a large image and the decode is abandoned rather than finished for
nobody; hand the viewer a file that wedges a decoder and the child is killed on
a timeout rather than taking a worker with it. The child is created suspended
inside an **AppContainer with no capabilities**, held in a job object that caps
its memory and kills it with the viewer, and handed the file's bytes on stdin
and never its path; the pixels come home through shared memory. See
[ADR 0009](docs/adr/0009-heavy-decodes-run-in-a-child.md) and
[ADR 0011](docs/adr/0011-the-decoder-loses-the-network.md).

The container is what closes the **network**, and closed is measured rather
than assumed, in both directions: a decoder taught to try cannot reach a live
listener waiting just outside the sandbox, and a listener it binds inside
accepts nothing while the same test hammers the port from outside. The
previous arrangement — a restricted token at low integrity — demonstrably
does not close a socket, whatever the common belief; it remains only as the
fallback for a machine that cannot register a container profile, and falling
back is reported rather than silent.

SVG is drawn for the size it is shown at, and drawn again when that changes, so
zooming in sharpens the picture instead of enlarging pixels. A document that
references another file does not get one: nitid refuses every href that is not
embedded, because an image is untrusted input and must not choose what the
viewer reads off the disk. Compressed `.svgz` is not opened — decompressing it
has no size limit to hide behind.

## Status

Early development — v0.19.0 is out. Startup, colour and format coverage hold:
every modern still format opens, a phone's photographs included, every one of
them reaches the screen without a wait, and the ones that animate play. The
process that decodes the heavy formats runs with no network in either
direction. HDR output works end to end: the surface follows the display's own
state while the viewer is open, and 10- and 12-bit sources cross the whole
pipeline at their own depth. Size is no longer a limit either — an image
past what a GPU texture can hold is drawn as tiles rather than crashing the
viewer. **And it is one window now**: opening a second image hands it to the
viewer already running instead of starting another, which is both faster and
what multi-select should have done all along. **And it has an interface now**:
a status line saying what is on screen, a toolbar that comes down when the
pointer reaches for it, and a key sheet — none of it in front of the
photograph. The framing can be held across a step for comparing a series, the
picture turned, and the backdrop behind transparency chosen, and `I` says what
the file says about itself. Development runs in small
versions, each one theme; the road to 1.0 is fixed:

| Version | What lands |
| --- | --- |
| ✅ v0.1.0 | Window, wgpu renderer, JPEG/PNG, zoom and pan, folder navigation |
| ✅ v0.2.0 | Instant startup — EXIF thumbnail first, background decode, prefetch |
| ✅ v0.3.0 | Color management: ICC via `moxcms`, sRGB and Display P3 |
| ✅ v0.4.0 | WebP, and one place that names every format |
| ✅ v0.4.1 | Untagged images pass through unconverted |
| ✅ v0.4.2 | JPEG XL |
| ✅ v0.5.0 | SVG, redrawn at the size it is shown |
| ✅ v0.6.0 | Sandboxed decoder process |
| ✅ v0.7.0 | HEIC, decoded in Rust |
| ✅ v0.8.0 | AVIF, decoded with `rav1d` |
| ✅ v0.9.0 | Decodes that can be stopped: cancelled on navigation, killed on a timeout |
| ✅ v0.10.0 | HEIC from its thumbnail, and its ICC colour on the GPU |
| ✅ v0.11.0 | The network closed to the decoder, and a cheaper bridge |
| ✅ v0.12.0 | Animation: GIF, APNG and animated WebP play |
| ✅ v0.13.0 | HDR output on Windows, following the display as it changes |
| ✅ v0.14.0 | A wider buffer: 10- and 12-bit sources through to the screen |
| ✅ v0.15.0 | Gigapixel images via tiled rendering |
| ✅ v0.16.0 | One window: a second launch hands its file over; multi-select arrives as one list |
| ✅ v0.17.0 | The interface: status line, a toolbar that appears on approach, key sheet, messages |
| ✅ v0.18.0 | Controlling the view: zoom lock across a step, viewing rotation, backdrop for transparency |
| ✅ v0.19.0 | The Info panel: EXIF, the place a photograph was taken, every row copyable |
| v0.20.0 – v0.35.0 | The everyday viewer: histogram and loupe, colour tools, clipboard, file operations, culling, comparison, settings |
| v0.36.0 – v0.39.0 | Windows integration: context menu, installer, auto-update, thumbnails |
| v0.40.0 – v0.41.0 | Documentation site, stabilisation |
| v1.0.0 | Public release — the default viewer, nothing missing |

Beyond 1.0: **2.x** makes a separate screenshot tool unnecessary — capture a
series, pick from it, hand it over in one action — with RAW at its tail. **3.x**
is the gallery: grid, folders, filters, timeline, duplicates.

## Building

```
cargo build --release
cargo run --release
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

Requires Rust 1.95 or newer — the version `egui-wgpu` needs, and the first that
builds against wgpu 30.

One tool beyond cargo is required: **NASM**, which `rav1d` needs to assemble
the AV1 decoder's kernels — without it the build fails rather than falling back
to something slower. `winget install NASM.NASM`, `scoop install nasm`, or your
platform's package manager.

## Installing

Download the zip from the [latest release](https://github.com/lacodda/nitid/releases),
unpack it anywhere, and run:

```
nitid install
```

This copies nitid to `%LOCALAPPDATA%\Programs\nitid` and registers the file
types it can open — no administrator, nothing outside your own user account.
Re-running it upgrades an existing install, even while the viewer is open.

The zip carries two executables and both are installed: `nitid.exe` is the one
to run from a terminal, and `nitidw.exe` is what the shell opens files with.
The second exists so that double-clicking an image never flashes a console
window — see [ADR 0004](docs/adr/0004-two-binaries-console-and-windowed.md).

Windows keeps the choice of default application to itself: no program is
allowed to seize a file type. After installing, nitid appears under **Open
with** — right-click an image, choose *Open with* → *Choose another app*, pick
nitid and tick *Always use this app*. It also shows up in *Settings → Apps →
Default apps*.

`nitid uninstall` removes both the files and the registration.

## Using it

```
nitid photo.jpg
```

Opening a file opens its folder: the arrow keys walk the images beside it.

| Key | Action |
| --- | --- |
| `←` `→` | previous / next image in the folder |
| `Home` `End` | first / last image |
| `Space` | pause / resume an animation; next image on a still |
| Wheel | zoom around the cursor |
| Drag | pan |
| Middle click | toggle fit and 100% |
| `0` `1` | fit to window / actual size |
| `F11` | fullscreen |
| `Esc` | quit |

"100%" means one image pixel per logical pixel, so a photo is the same size
here as everywhere else on a scaled display.

### Environment

| Variable | Effect |
| --- | --- |
| `NITID_STARTUP_REPORT=1` | print the startup breakdown to stderr, and state the surface each time it is configured |
| `NITID_EXIT_AFTER_FIRST_FRAME=1` | close as soon as a picture is on screen; used by the startup test |
| `NITID_TILE_LIMIT=<pixels>` | lower the texture side an image is cut into tiles at, so the tiled path can be exercised on a small file; never raises it past what the device accepts |
| `NITID_NO_SINGLE_INSTANCE=1` | open a window of this launch's own instead of handing the file to one already open; used by the startup gate, which measures a cold start |
| `NITID_INSTANCE_ID=<text>` | share a window only with launches carrying the same value, so a test never talks to the viewer you have open |

## Design notes

Two decisions shape everything else:

**nitid owns its swapchain.** HDR output needs a surface format and color space chosen deliberately — `ExtendedSrgbLinear` on `Rgba16Float`, and re-chosen while the viewer runs as the display changes. A GUI framework that configures the surface for you closes that door, so the window and renderer are ours; `egui` is used for widgets only.

**Untrusted input is isolated, and slow input is interruptible.** An image decoder parses hostile data by definition — pictures arrive from the internet. Every decoder nitid ships is Rust, so a malformed file causes a panic or an error rather than code execution. The separate low-integrity process that was built for memory safety earns its keep for a different reason: a thread cannot be stopped and a process can, so a decode is abandoned when you navigate away and killed when it wedges.

Architecture decisions are recorded in [`docs/adr/`](docs/adr/).

## License

MIT — see [LICENSE](LICENSE).
