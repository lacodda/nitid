# 0002 — C-backed decoders run in a sandboxed process

Date: 2026-08-09
Status: accepted, narrowed by [ADR 0007](0007-heic-decodes-in-rust.md)

> **Narrowed 2026-08-19.** The premise below — that HEIC has no viable Rust
> decoder — stopped holding: `heif-oxide` decodes it in Rust, and v0.7.0 opens
> HEIC in-process without this boundary. Everything here still stands for AVIF,
> which is the format the sandbox now waits for. See ADR 0007.

## Context

An image decoder parses untrusted input as its normal mode of operation: files arrive from the internet, from messengers, from email attachments. Historically this is one of the most productive sources of remote code execution — a crafted file overflows a buffer inside the decoder and the attacker gains execution. CVEs in libheif, libraw and ImageMagick are regular events, not anomalies.

Pure-Rust decoders bound the damage: a malformed file causes a panic, not arbitrary execution. Coverage in Rust turns out to be broad — JPEG (`zune-jpeg`), PNG/GIF/TIFF/BMP (`image`), WebP (`image-webp`), JPEG XL (`jxl-oxide`), SVG (`resvg`).

Two formats have no viable Rust decoder: HEIC (`libheif-rs`) and AVIF (`libavif`). HEIC is every photo taken on an iPhone, so dropping it is not an option. `libavif` additionally has not been updated since July 2024.

## Decision

Pure-Rust decoders run in the main process.

C-backed decoders run in a separate process with a restricted token: a job object, low integrity level, and no path to the file at all — the bytes are handed over on a pipe. Decoded pixels are returned the same way. A crash in that process surfaces as "could not open this file" while the viewer stays alive.

## Addendum, 2026-08-14 (v0.6.0, when the boundary was built)

Two corrections to the above, both found while implementing it:

- **"No network access" was wrong as written.** A low integrity process can still open a socket; the belief that integrity level closes the network is common and false. Shutting it needs an AppContainer with no capabilities, or a firewall rule naming the executable. Neither is in v0.6.0: every decoder behind the boundary today is pure Rust and none opens a socket, so the gap costs nothing yet — but it is a gap, and it is recorded rather than papered over. It must be closed before HEIC and AVIF arrive in v0.7.0, since those are the C libraries the boundary exists for.
- **The child is confined by mutating its own token, not by being given one.** `CreateProcessAsUser` needs privileges an ordinary desktop process does not hold. Creating the process suspended, stripping the token it already has, assigning the job, and only then resuming achieves the same confinement without elevation — and closes the race where a process runs before its limits apply.

The boundary was built one stage before the decoders that need it, so adding HEIC is adding a decoder rather than a decoder and an architecture at once. `Format::needs_sandbox` is the switch; nothing answers true yet.

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
