#!/usr/bin/env bash
# Check a platform-split module the way a non-Windows build would see it.
#
# Why this exists: this project is developed on Windows, where `cfg(windows)`
# is always true, so a module whose Windows half is behind a cfg and whose
# callers are not compiles fine here and breaks on Linux. That happened — six
# names with nothing behind them reached main, and only CI could see it.
#
# A proper cross-check (`cargo check --target x86_64-unknown-linux-gnu`) does
# not get that far: `rav1d` builds hand-written assembly and its build script
# needs a Linux linker, so the check stops long before this project's code.
#
# So the module is compiled on its own instead, with the cfgs rewritten:
# `cfg(windows)` becomes `cfg(any())` (never) and `cfg(not(windows))` becomes
# `cfg(all())` (always). Crude, and enough to catch the two things that
# actually go wrong — an import left behind, and a caller naming a function
# that the other platform does not offer.
#
# Limits, said plainly. The module is compiled alone, so it is checkable only
# if it reaches for nothing but `std` and `anyhow`: `src/files.rs` qualifies,
# `src/clipboard.rs` does not (it imports `crate::image_source`, and this
# check reports that as an unresolved import rather than as a real fault).
# `mod` declarations inside a module are not followed either, and a clean
# result here does not replace CI — it catches the one class of mistake this
# machine cannot see at all.
#
# Usage: tools/check-non-windows.sh src/files.rs [more.rs ...]
set -eu

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <module.rs> [more.rs ...]" >&2
  exit 2
fi

dep_dir="target/debug/deps"
anyhow_rlib="$(ls -t "$dep_dir"/libanyhow-*.rlib 2>/dev/null | head -1 || true)"
if [ -z "$anyhow_rlib" ]; then
  echo "no built anyhow to link against — run 'cargo build' first" >&2
  exit 2
fi

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

status=0
for module in "$@"; do
  name="$(basename "$module")"

  python - "$module" "$out/$name" <<'PY'
import io, re, sys

source, destination = sys.argv[1], sys.argv[2]
text = io.open(source, encoding="utf-8").read()
text = text.replace("#[cfg(not(windows))]", "#[cfg(all())]")
text = text.replace("#[cfg_attr(not(windows), ", "#[cfg_attr(all(), ")
text = text.replace("#[cfg(windows)]", "#[cfg(any())]")
# Tests that only run on Windows go with it; they are the module's own, and
# compiling them here would ask for a test harness this check does not build.
text = re.sub(r"#\[cfg\(test\)\]\n#\[cfg\(any\(\)\)\]\nmod tests \{.*", "", text, flags=re.S)
io.open(destination, "w", encoding="utf-8").write(text)
PY

  if rustc --edition 2024 --crate-type lib \
    --extern "anyhow=$anyhow_rlib" \
    -L "dependency=$dep_dir" \
    --deny warnings \
    -o "$out/lib$(basename "$name" .rs).rlib" \
    "$out/$name"; then
    echo "ok: $module compiles clean with windows off"
  else
    echo "FAILED: $module does not compile with windows off" >&2
    status=1
  fi
done

exit "$status"
