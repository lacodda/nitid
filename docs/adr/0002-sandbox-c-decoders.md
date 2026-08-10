# 0002 — C-backed decoders run in a sandboxed process

Date: 2026-08-09
Status: accepted

## Context

An image decoder parses untrusted input as its normal mode of operation: files arrive from the internet, from messengers, from email attachments. Historically this is one of the most productive sources of remote code execution — a crafted file overflows a buffer inside the decoder and the attacker gains execution. CVEs in libheif, libraw and ImageMagick are regular events, not anomalies.

Pure-Rust decoders bound the damage: a malformed file causes a panic, not arbitrary execution. Coverage in Rust turns out to be broad — JPEG (`zune-jpeg`), PNG/GIF/TIFF/BMP (`image`), WebP (`image-webp`), JPEG XL (`jxl-oxide`), SVG (`resvg`).

Two formats have no viable Rust decoder: HEIC (`libheif-rs`) and AVIF (`libavif`). HEIC is every photo taken on an iPhone, so dropping it is not an option. `libavif` additionally has not been updated since July 2024.

## Decision

Pure-Rust decoders run in the main process.

C-backed decoders run in a separate process with a restricted token: a job object, low integrity level, no network access, and access to the single file being decoded. Decoded pixels are returned over IPC. A crash in that process surfaces as "could not open this file" while the viewer stays alive.

## Consequences

Positive:

- A memory-safety bug in a C library cannot compromise the viewer.
- Full format coverage including iPhone photos.
- The same process boundary keeps heavy decodes off the UI thread — an architecture chosen for safety also solves responsiveness.

Negative:

- Two to three days of IPC machinery before HEIC works at all.
- 5–15 ms of overhead per file, negligible against HEIC decode time.
- A second executable to ship, sign and update.

Rejected alternatives:

- **Everything in-process** — simplest and marginally faster, but a bug in C code takes down the application and, in the worst case, executes attacker-controlled code with the user's privileges.
- **Rust-only, no C at all** — maximum safety and the simplest cross-platform build, but no HEIC and no RAW; iPhone photos are too common to exclude.
