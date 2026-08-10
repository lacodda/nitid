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

## Status

Early development — v0.1.0 is out; the picture shows up and moves, but the
color management and the fast start that justify the product are still ahead.
The version map to 1.0 is fixed:

| Version | What lands |
| --- | --- |
| ✅ v0.1.0 | Window, wgpu renderer, JPEG/PNG, zoom and pan, folder navigation |
| v0.2.0 | Instant startup — EXIF thumbnail first, background decode, prefetch |
| v0.3.0 | Color management: ICC via `moxcms`, sRGB and Display P3 |
| v0.4.0 | WebP, JPEG XL, GIF, TIFF, BMP, SVG |
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

Requires Rust 1.88 or newer.

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

## Design notes

Two decisions shape everything else:

**nitid owns its swapchain.** HDR output on Windows is only reachable through `Bt2100Pq` on the `Rgb10a2Unorm` format — DirectX 12 has no extended-sRGB swapchain color space. A GUI framework that configures the surface for you closes that door, so the window and renderer are ours; `egui` is used for widgets only.

**Untrusted input is isolated.** An image decoder parses hostile data by definition — pictures arrive from the internet. Pure-Rust decoders run in-process, where a malformed file causes a panic rather than code execution. HEIC and AVIF exist only as C libraries, so from v0.5.0 they run in a separate low-integrity process: a crash there means "could not open this file", not a compromised viewer.

Architecture decisions are recorded in [`docs/adr/`](docs/adr/).

## License

MIT — see [LICENSE](LICENSE).
