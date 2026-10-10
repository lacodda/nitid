#!/usr/bin/env bash
# Lint the crate the way the Linux CI job does, from a Windows machine.
#
# Why this exists: this project is developed on Windows, where `cfg(windows)`
# is always true. Code whose only caller sits behind that cfg compiles clean
# here and is dead code on Linux, which `-D warnings` turns into a failed
# build. It happened three times running - six names with nothing behind them
# in v0.22, a stub that read none of its fields in v0.23, a constant named
# only by the Windows half in v0.37 - and each time only CI saw it, after the
# tag.
#
# Clippy for the Linux target gets that far once one thing is found: `rav1d`
# builds its assembly in a build script, which archives the objects with the
# target's `ar`. Windows has none, but rustup's `llvm-tools` component ships
# `llvm-ar`, which writes the archive any target wants. Nothing is linked -
# clippy only checks - so no Linux linker is needed.
#
# Needs, each said when missing: the `x86_64-unknown-linux-gnu` target, the
# `llvm-tools` component, and NASM (which the Windows build needs anyway).
set -eu

target="x86_64-unknown-linux-gnu"
host="$(rustc -vV | sed -n 's/^host: //p')"
ar="$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-ar"
if [ ! -x "$ar" ] && [ ! -x "$ar.exe" ]; then
  echo "llvm-ar is missing: rustup component add llvm-tools" >&2
  exit 2
fi
if ! rustup target list --installed | grep -qx "$target"; then
  echo "the Linux target is missing: rustup target add $target" >&2
  exit 2
fi
if ! command -v nasm >/dev/null 2>&1; then
  echo "nasm is missing: rav1d assembles the AV1 decoder with it" >&2
  exit 2
fi

AR_x86_64_unknown_linux_gnu="$ar" cargo clippy --all-targets --target "$target" -- -D warnings
