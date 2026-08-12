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

## Formats

Everything below decodes in pure Rust, in this process: a malformed file costs
an error, never code execution.

| Format | Extensions | Embedded ICC profile |
| --- | --- | --- |
| JPEG | `.jpg` `.jpeg` `.jpe` `.jfif` | yes |
| PNG | `.png` | yes |
| WebP | `.webp` | yes |
| GIF | `.gif` | sRGB by definition |
| BMP | `.bmp` | no |
| TIFF | `.tif` `.tiff` | no |

The format is decided by the bytes, not the extension: a `.png` that is really
a JPEG opens rather than erroring. HEIC and AVIF need C libraries and arrive in
v0.5.0 behind a sandbox; JPEG XL and SVG follow in v0.4.2.

## Status

Early development — v0.4.1 is out. Startup, colour and pure-Rust format
coverage all hold. HDR is still ahead. The version map to 1.0 is fixed:

| Version | What lands |
| --- | --- |
| ✅ v0.1.0 | Window, wgpu renderer, JPEG/PNG, zoom and pan, folder navigation |
| ✅ v0.2.0 | Instant startup — EXIF thumbnail first, background decode, prefetch |
| ✅ v0.3.0 | Color management: ICC via `moxcms`, sRGB and Display P3 |
| ✅ v0.4.0 | WebP, and one place that names every format |
| ✅ v0.4.1 | Untagged images pass through unconverted |
| v0.4.2 | JPEG XL, SVG |
| v0.5.0 | Sandboxed C decoders — HEIC and AVIF |
| v0.6.0 | HDR output on Windows (`Bt2100Pq` on `Rgb10a2Unorm`) |
| v0.7.0 | Toolbar, thumbnail strip, settings |
| v0.8.0 | Shell integration, installer, auto-update |
| v0.9.0 | RAW via `rawler` |
| v1.0.0 | Public release |

## Building

```
cargo run --release
cargo fmt --check && cargo clippy -- -D warnings && cargo test
```

Requires Rust 1.89 or newer.

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
| `NITID_STARTUP_REPORT=1` | print the startup breakdown to stderr |
| `NITID_EXIT_AFTER_FIRST_FRAME=1` | close as soon as a picture is on screen; used by the startup test |

## Design notes

Two decisions shape everything else:

**nitid owns its swapchain.** HDR output on Windows is only reachable through `Bt2100Pq` on the `Rgb10a2Unorm` format — DirectX 12 has no extended-sRGB swapchain color space. A GUI framework that configures the surface for you closes that door, so the window and renderer are ours; `egui` is used for widgets only.

**Untrusted input is isolated.** An image decoder parses hostile data by definition — pictures arrive from the internet. Pure-Rust decoders run in-process, where a malformed file causes a panic rather than code execution. HEIC and AVIF exist only as C libraries, so from v0.5.0 they run in a separate low-integrity process: a crash there means "could not open this file", not a compromised viewer.

Architecture decisions are recorded in [`docs/adr/`](docs/adr/).

## License

MIT — see [LICENSE](LICENSE).
