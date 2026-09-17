# Contributing to nitid

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

## The gate

`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test` is
what CI runs; a green terminal here means a green pull request.

## Architecture decisions

Anything that would be asked about again later is written down in
[`docs/adr/`](https://github.com/lacodda/nitid/tree/main/docs/adr) as a short
Context / Decision / Consequences note, in the same commit as the change.

## Commits

Conventional Commits, English, no trailers. Breaking changes are declared in the
commit footer - see
[ADR 0006](https://github.com/lacodda/nitid/blob/main/docs/adr/0006-breaking-changes-come-from-commit-footers.md).
