---
title: Command line
description: Every command nitid takes from a prompt - opening pictures, installing, the desktop's picture, describing pictures and converting them.
---

Run these with `nitid`, the console binary. `nitidw` is the one Explorer
starts; given a command that prints, it hands it to `nitid`.

```
nitid [FILE...]
nitid install
nitid uninstall
nitid wallpaper [FOLDER] [--every 30m] [--only PATTERN] [--fit fill|fit]
nitid wallpaper next | off
nitid info FILE... [--json]
nitid convert FILE... --to jpeg|png|webp [--quality 1-100] [--srgb] [--out FOLDER]
nitid --help
nitid --version
```

## Naming pictures

`info` and `convert` take any mix of:

- **a picture** - itself, whatever it is called;
- **a folder** - the pictures in it, the ones nitid would walk, without its
  subfolders;
- **a pattern** - `*.heic`, `IMG_????.jpg`, or several at once as
  `*.jpg;*.png` - over the pictures of its folder, ignoring case.

nitid expands the pattern itself: neither cmd nor PowerShell does it for the
programs they start, so `nitid convert *.heic --to jpeg` works the same in
every shell. A picture reached two ways is worked on once. An argument that
names nothing is said on the error stream, and the rest still goes ahead.

Several pictures are worked on at once, up to four, and reported in the order
they were named.

## `nitid info`

What each picture says about itself - the same facts as the Info panel (`I`)
and the colour passport (`K`), read by the same code:

```
$ nitid info IMG_0412.jpg
IMG_0412.jpg
  Size         3024 x 4032
  Format       JPEG
  Depth        8-bit
  File         2.9 MB
  Orientation  turned a quarter clockwise (EXIF 6)

  Colour
  Profile      Display P3
  Primaries    Display P3
  Transfer     sRGB curve
  ICC          version 4.0, display profile for RGB, perceptual intent

  Camera
  Camera       Apple iPhone 13
  Exposure     1/120 s
  Aperture     f/1.6
  ISO          50
  Focal length 5.1 mm (26 mm eq.)
  Taken        2026-08-28 14:03:11.204

  Place
  Coordinates  51.477900, -0.001500
```

The size is the size it is shown at, after the orientation. *Primaries* names
the standard space the profile's primaries belong to - sRGB, Display P3,
Adobe RGB (1998), BT.2020, DCI-P3, ProPhoto RGB - or says *its own* for a
calibrated monitor's or a camera's. *Transfer* names the curve by how it
behaves, so an sRGB curve stored as a table and one stored as five numbers
both read *sRGB curve*. A file with no profile says so: it is shown exactly as
stored, as every browser shows it.

### `--json`

The same facts as values, for a script: one array, an object per picture, in
the order named.

| Field | Value |
| --- | --- |
| `path` | the picture, as named |
| `format` | `JPEG`, `PNG`, `WebP`, `JPEG XL`, `HEIC`, `AVIF`, `SVG`, `GIF`, `BMP`, `TIFF` |
| `width`, `height` | the size as shown, in pixels |
| `depth` | `8` or `16` bits a channel |
| `bytes` | the file's size |
| `orientation` | the EXIF number, `1` (upright) to `8` |
| `frames` | `1` for a still picture |
| `mark` | `"kept"`, `"rejected"` or `null` |
| `colour` | `null` when untagged, or `profile`, `primaries`, `transfer`, `hdr`, `version`, `class`, `space`, `intent` |
| `caveat` | what the file states that nitid cannot honour, or `null` |
| `camera` | `camera`, `lens`, `exposure` (seconds), `aperture`, `iso`, `compensation` (stops), `focal_length` (mm), `focal_length_35mm`, `taken` - each `null` when the file does not say |
| `place` | `latitude` and `longitude` in decimal degrees, or `null` |

A picture that cannot be read is an object with `path` and `error` instead,
so every name asked about has its answer. The output is ASCII whatever the
names are - anything else is escaped - so Windows PowerShell 5.1, which
reads a program's output in the console's code page, gets every name intact:

```powershell
nitid info *.jpg --json | ConvertFrom-Json | Where-Object { $_.colour.primaries -eq 'Display P3' }
```

## `nitid convert`

Each picture written again as JPEG, PNG or WebP, beside it, or in `--out
FOLDER` (made if it is not there):

```
$ nitid convert *.heic --to jpeg
IMG_0412.heic -> IMG_0412.jpg
IMG_0413.heic -> IMG_0413.jpg
converted 2 of 2
```

It writes through the same path as `Ctrl+Shift+S`, so a converted picture is
what a save from the window would be: the file's colour profile written into
it - or, with `--srgb`, the colour converted into sRGB for somewhere that
ignores profiles - an HDR picture as a standard-range screen shows it, and
the picture turned the way it is shown. What a format cannot carry is said
under the line, in the save box's words: the transparency a JPEG fills with
white, the bits past eight, the frames past the first.

Unlike a save, a conversion is the same photograph in another container, so
it keeps what the photograph says about itself: the EXIF block (camera, date,
place - with the orientation set upright, because the pixels now are), a
kept or rejected mark, and the date the file was last written, so a folder
sorted by date still has the pictures where they were. To send a picture
without the camera and the place, make a clean copy with `Ctrl+M`.

**Nothing is replaced.** A name already taken is left alone and said so -
which also means a second run over the same folder finishes what an
interrupted one started. `--quality` sets JPEG and WebP quality, 90 unless
given.

## Exit codes

`0` when everything named was done; `1` when anything was not - a name that
matched nothing, a file that would not decode, a mistake on the command line.
A picture left alone because its new name was taken is not a failure.

## Installing, and the desktop's picture

`nitid install` and `nitid uninstall` are described in
[Getting Started](/nitid/getting-started/); `nitid wallpaper` in
[The desktop's picture](/nitid/guides/the-desktop-picture/).
