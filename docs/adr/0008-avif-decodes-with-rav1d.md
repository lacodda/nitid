# 8. AVIF decodes with rav1d, and NASM joins the build

Date: 2026-08-19

## Status

Accepted. Finishes narrowing [ADR 0002](0002-sandbox-c-decoders.md), which
[ADR 0007](0007-heic-decodes-in-rust.md) began.

## Context

ADR 0002 built a sandbox for the two formats that had no Rust decoder: HEIC and
AVIF. HEIC turned out to have one (ADR 0007). AVIF was the remaining reason for
the boundary, and this stage set out to put it behind one.

Every route was measured by building it, not read about:

| Route | Result |
| --- | --- |
| `oxideav-avif` + `oxideav-av1` | Builds in pure Rust, and refuses every real file — three fixtures from two independent encoders (libavif, libaom) all fail with "unexpected end of OBU bytestream". The container parser is real; the AV1 pixel decode is a stub that returns `Unsupported`. |
| `avif-decode` | Needs cmake. |
| `dav1d` (the binding crate) | Needs pkg-config and a system libdav1d, itself built with meson and ninja. |
| `libavif-sys` → `libdav1d-sys` | Needs meson and ninja. |
| `rav1d` | Builds, and decodes every fixture correctly — but only with NASM installed. Without it the crate does not compile at all; the `asm` feature is not optional in practice. |

`rav1d` is dav1d translated to Rust by the ISRG. Two things follow from that,
and they pull in opposite directions: the memory-safety argument for the
sandbox is much weaker, because a malformed file meets Rust rather than C; and
the translation is mechanical, so the crate carries a great deal of `unsafe`
internally and is not the same thing as code written safely from the start.

It also exposes dav1d's C interface rather than a Rust one, so calling it means
writing the `unsafe` bridge here.

## Decision

AVIF decodes with `rav1d` for the AV1 payload and `avif-parse` for the
container, in the viewer's own process. `Format::needs_sandbox()` still answers
false for everything.

NASM becomes a build requirement, installed in CI and named in the manifest
comment beside the dependency. This is the first tool the project needs beyond
cargo, and it is accepted deliberately: the alternative routes each need *more*
(cmake, or meson and ninja, or a system library), and one of them downloads a
binary from a third party during the build.

The `unsafe` bridge lives in its own module, `src/avif.rs`, so the rest of the
viewer sees bytes going in and pixels coming out. Every `unsafe` block carries
its safety argument, the decoder context and the picture are released by guards
that run on error paths as well as success, and a test decodes the same file
thirty-two times over to catch a leak of either.

Unlike HEIC, **colour is not resolved on the way in**. The frame comes back as
YUV, this module converts it with exactly the coefficients the bitstream
declares, and the primaries and transfer curve are handed on as a profile for
the shader. AVIF therefore gets the colour management the product promises, and
a Display P3 AVIF is shown across the display's gamut rather than folded into
sRGB.

## Consequences

Two things were found in the process that are worth recording, because both
were defects rather than decisions:

- **`avif-parse` panics on damaged input.** It asserts its way through some
  malformed files rather than returning an error, so a corrupted AVIF from the
  internet would take the viewer down. The parse is wrapped in
  `catch_unwind` and reported as a file that would not open. The decoder itself
  needs no such net.
- **The container's rotation is not in EXIF.** libavif records a rotated image
  as an `irot` property and writes no EXIF item at all, so a viewer reading only
  EXIF shows such a picture on its side. `avif-parse` does not surface `irot` or
  `imir`, so they are read here — which means walking the box tree, and
  therefore knowing that `meta` is a FullBox whose four bytes of version and
  flags would otherwise be read as the length of the box after it. That mistake
  was made and caught by the test against a real file.

What this costs:

- NASM in the build. Anyone building from source needs it, and CI installs it in
  three workflows.
- 10- and 12-bit AVIF are refused with a reason rather than shown narrowed to
  eight bits. The renderer uploads RGBA8; the wider buffer arrives with HDR.
- `rav1d` is a large dependency with a great deal of internal `unsafe`. The
  sandbox from ADR 0002 remains built and unused, and `needs_sandbox()` is one
  word if that judgement ever changes.
