<p align="center"><img src="https://github.com/lacodda/nitid/raw/main/assets/banner.svg" alt="nitid - a fast image viewer with honest color" width="720"></p>

> Double-click a file and the picture is already there - in the colours the photographer chose, with the headroom the display was bought for.

<p align="center">
  <a href="https://github.com/lacodda/nitid/releases/latest"><img src="https://img.shields.io/github/v/release/lacodda/nitid?style=flat-square" alt="Release"></a>
  <a href="https://github.com/lacodda/nitid/actions"><img src="https://img.shields.io/github/actions/workflow/status/lacodda/nitid/ci.yml?style=flat-square" alt="CI"></a>
  <a href="https://github.com/lacodda/nitid/blob/main/LICENSE"><img src="https://img.shields.io/github/license/lacodda/nitid?style=flat-square" alt="License"></a>
</p>

## Why

Opening an image should not be an event. Most viewers make it one: a white
flash, a spinner, a second of nothing while a 24-megapixel JPEG decodes. And
when the picture finally lands, the colours are often not the ones in the file
- the profile it carries was ignored, so the same photograph looks oversaturated
here and correct in the editor it came from.

nitid closes both. The embedded thumbnail is on screen in single-digit
milliseconds while the full image decodes behind it, and the file's colour
profile is converted against the display's own profile in the shader, every
frame, for free. On an HDR display the output is extended-range linear light,
so highlights drive the headroom instead of clipping at white.

`nitid` is Latin for *clear, bright, sharp* - the name describes the
difference, not the function.

## What you get

- **The picture before the decode finishes.** Thumbnail first, full image
  behind it, neighbours prefetched - arrow keys never wait.
- **Colours that mean something.** ICC on the GPU; a file with no profile is
  passed through untouched, exactly as every browser shows it.
- **HDR that follows the display.** Turn it on or off in Windows and the
  surface is reconfigured without a restart.
- **Every modern still format**, decoded in pure Rust - JPEG, PNG, WebP, JPEG
  XL, HEIC, AVIF, SVG, GIF, BMP, TIFF - and the animated ones play.
- **Tools for reading a picture**, not just looking at it: histogram, loupe,
  clipping warning, an eyedropper that reads in the file's terms and the
  display's.
- **One window.** Opening a second image hands it to the viewer already
  running, so a multi-select does not scatter windows across the desktop.
- **Nothing that phones home.** The process that decodes the heavy formats has
  no network in either direction.

## A day in the life

```console
$ nitid photo.heic
```

The window is up in about 11 ms, the picture on screen at about 128 ms - from
process start, on a 24-megapixel file. Run with `NITID_STARTUP_REPORT=1` and it
says so for your own machine:

```
window created at    11 ms
gpu ready at        118 ms
thumbnail up at     128 ms   <- the picture is on screen here
first pixels in     146 ms
nitid: surface Rgba16Float ExtendedSrgbLinear, display headroom 7.71x
```

That figure is held by `tests/startup.rs`, so a change that puts a full decode
back on the startup path fails the build rather than quietly costing a tenth of
a second.

From there, arrow keys walk the folder, `?` shows every key, `I` says what the
file says about itself, `H` draws a histogram of its own values, and `P` reads
the pixel under the pointer in both the file's terms and the display's.

## Install

Download the zip from the [latest release](https://github.com/lacodda/nitid/releases/latest),
unpack it anywhere, and run:

```
nitid install
```

This copies nitid to `%LOCALAPPDATA%\Programs\nitid`, registers the file types
it opens, puts that directory on your `PATH`, and leaves a desktop shortcut -
no administrator, nothing outside your own user account. `nitid uninstall`
removes all of it.

Windows keeps the choice of default application to itself, so after installing,
pick nitid under **Open with -> Choose another app** and tick *Always use this
app*. Full instructions: [Getting Started](https://lacodda.github.io/nitid/getting-started/).

## Status

v0.31.0, in daily use on Windows. Startup, colour and HDR hold end to end;
every modern still format opens and the animated ones play; images past what a
GPU texture can hold are drawn as tiles. What landed in each version:
[CHANGELOG](https://github.com/lacodda/nitid/blob/main/CHANGELOG.md).

## Documentation

**[lacodda.github.io/nitid](https://lacodda.github.io/nitid/)** - keys, formats,
colour and the decisions behind them. Architecture decision records are in
[`docs/adr/`](https://github.com/lacodda/nitid/tree/main/docs/adr).

Building it yourself: [CONTRIBUTING.md](https://github.com/lacodda/nitid/blob/main/CONTRIBUTING.md).

## License

MIT (c) [Kirill Lakhtachev](https://lacodda.com)
