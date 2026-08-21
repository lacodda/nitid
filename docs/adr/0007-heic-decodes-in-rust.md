# 7. HEIC decodes in Rust, outside the sandbox

Date: 2026-08-19

## Status

Accepted. Narrows [ADR 0002](0002-sandbox-c-decoders.md) without replacing it.

> **Two of the consequences below were settled in v0.10.0**, and one of them
> turned out to be worse than described.
>
> The missing quick frame is fixed: a HEIC does carry a thumbnail, as an item
> in its container rather than in EXIF, and nitid now shows it — about 5 ms
> against 470 ms for a 12-megapixel photograph.
>
> The colour limitation was two things. For a file stating CICP codes it is as
> written: the decoder resolves the colour and the gamut is lost. For a file
> carrying an **ICC profile** instead, the decoder was producing *wrong*
> pixels, not merely narrow ones — it notices the profile, does not read it,
> and falls back to matrix coefficients the image was not coded with. That path
> now rewrites the `colr` box in a copy so the decoder agrees with libheif, and
> passes the ICC profile to the shader. Such files get the colour management
> the rest of the viewer gives; CICP files still do not.

## Context

ADR 0002 built a sandbox for HEIC and AVIF on a premise: both formats exist
only as C libraries, where a malformed file is a memory-safety bug rather than
a panic. The boundary shipped in v0.6.0 with nothing behind it, waiting for
those decoders.

By the time this stage started, the premise had stopped being true for HEIC.
`heif-oxide` decodes the ISOBMFF container and the HEVC payload in Rust, with
no C dependency at all. Checked against libheif on the same files, it returns
the same pixels.

The C route was measured rather than assumed, and every path through it needs a
build tool this project does not otherwise require:

| Route | What it needs to build |
| --- | --- |
| `libheif-sys` (even with `embedded-libheif`) | vcpkg |
| `heif-rs` | libclang, plus a download from a third-party GitHub release during the build |
| `libavif-sys` → `libdav1d-sys` | meson and ninja |
| `rav1d` | NASM (it does not compile without the `asm` feature) |

Any of those becomes a requirement for CI and for everyone building from
source, and the `heif-rs` route additionally makes the build fetch a binary
from a repository that is not ours.

Against that, the Rust decoder has one real cost, and it is not a build
concern: **it delivers sRGB**. It reads the file's colour description and
converts the pixels itself rather than handing them over with a profile
attached. A photograph tagged Display P3 therefore arrives already converted,
and the viewer has nothing left to apply on the GPU.

## Decision

HEIC decodes with `heif-oxide`, in the viewer's own process. `Format::Heic`
answers `needs_sandbox()` false, and the sandbox stays empty.

The pixels are accepted as sRGB and no profile is attached to them:
`color::raw_profile` returns `None` for HEIC deliberately, because attaching
the file's profile would convert an already-converted image a second time.

Two consequences of the decoder are recorded here rather than left to be
discovered:

- **Orientation comes from the container.** `heif-oxide` applies the `irot` and
  `imir` properties itself, and an encoder writing a photograph turns the EXIF
  orientation into exactly those properties. Both descriptions sit in the same
  file, so `Format::orients_itself()` answers true for HEIC and the EXIF tag is
  ignored — honouring both would turn a portrait photograph on its side.
- **The first frame waits for the whole decode.** There is no embedded
  thumbnail on this path: the crate reads the primary item only, not the
  container's `thmb` item, and a HEIC carries no EXIF thumbnail for the JPEG
  path to find. A 12-megapixel HEIC takes around a second to reach the screen
  against 40 ms for the equivalent JPEG. `tests/startup.rs` gates the two
  separately so the difference is a measured number rather than an impression.

## Consequences

The format a modern phone photographs in opens, and adding it added two crates
and no build tools. The sandbox built in v0.6.0 was not wasted: AVIF still has
no Rust decoder that produces pixels, and it is the format that will use the
boundary.

What this costs, plainly:

- A Display P3 photograph is shown inside sRGB rather than across the display's
  full gamut. On the wide-gamut screen this viewer was written for, that is
  visible — it is the one place in nitid where colour is resolved on the way in
  instead of in the shader, and it contradicts the ordinary rule that decoded
  pixels stay as the file stored them.
- Opening a HEIC is roughly a second slower than opening a JPEG of the same
  photograph, and the promise of a picture before the wait is noticed does not
  hold for this format yet.

Both are entered in `План.md` as work, not accepted as the finished state.
Either is fixed the same way: by reaching the decoder's YUV output before it is
converted, which needs `heif-oxide` to expose it or nitid to decode the HEVC
itself. Nothing in this decision blocks that — the profile is read from a
container this project already parses, and `ColorTransform` applies whatever it
is given.

The decoder is young (0.1.0). It is held to the same standard as the rest: a
malformed file must be an error rather than a crash, which
`a_broken_heic_is_an_error_rather_than_a_panic` checks against truncated and
corrupted input. If it ever proves unsound in a way Rust does not catch, the
boundary from ADR 0002 is already built and `needs_sandbox()` is one word.
