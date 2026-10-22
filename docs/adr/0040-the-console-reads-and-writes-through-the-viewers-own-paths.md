# 40. The console reads and writes through the viewer's own paths

Date: 2026-10-22

## Status

Accepted.

## Context

v0.40 gives `nitid.exe` two commands that work on pictures rather than on the
installation: `info`, which says what a picture is, and `convert`, which writes
it again as JPEG, PNG or WebP. ADR 0004 made the console binary able to print;
these are the first commands that use it for anything a script would read.

Three things had to be settled: whose code answers, what the output is for,
and what a conversion carries.

## Decision

**No second implementation of anything.** `info` decodes with the decoder the
window uses — through the sandboxed child for HEIC and AVIF — and reads EXIF
with the same reader, so the panel and the command cannot disagree about a
file. `convert` writes through `export::encode`, the save box's path, which is
why that path had to be put right first (ADR 0039). The only new knowledge is
`color::describe`, which names a profile's primaries and curve by how they
behave: by whether the viewer would convert between the profile and a
standard space at all, and by sampling the curve.

**Patterns are expanded by nitid.** cmd and PowerShell hand `*.heic` to a
program as typed. A command whose own documentation says
`nitid convert *.heic` must therefore expand it, and it does so by the rules
the viewer walks a folder by (`folder::pictures`, `pattern::Pattern`), so a
pattern finds exactly the files the window would show.

**Text and JSON, both.** `info` prints text for a person and, with `--json`,
values for a script — seconds rather than `1/250 s`, the EXIF number rather
than "a quarter clockwise", `null` rather than a missing line. The rule
against settings does not apply: these are two readers at once, and a
script parsing the text would break on the first reworded label. The JSON is
written by hand (`json`), as ASCII with everything else escaped, because
Windows PowerShell 5.1 reads a program's output in the console's code page
and mangles UTF-8.

**A conversion is the same photograph in another container.** So, unlike a
save, it keeps the EXIF block (with the orientation set upright, since the
pixels are turned), a cull mark (a keep rides in the EXIF; a reject, which
lives in XMP, is written after), and the source's modification time — the
viewer's own "newest picture" and every date-sorted folder go by it. It keeps
the profile by default and bakes to sRGB on `--srgb`, the save box's two
choices.

**Nothing is replaced.** The new name must not exist, checked before the
decode so a re-run is cheap and enforced by `create_new` at the write so two
pictures of one name converting at once cannot both win. A partial write is
removed. A taken name is reported and is not a failure, which makes re-running
an interrupted conversion the way to finish it.

**Up to four pictures at a time, reported in order.** A decoded 48-megapixel
photograph at sixteen bits is about 400 MB, so the count is capped rather than
set by the processor; results are held until those named before them are
out, so output pairs with input.

## Consequences

- `info` costs a full decode per file. A header-only reader would answer
  faster for JPEG and PNG and could not answer at all for AVIF and JPEG XL,
  whose colour is read by the decoder. The decode is the honest answer.
- A photograph that had to be turned loses its EXIF thumbnail: the thumbnail
  was stored turned, and with the tag now upright it would show the picture
  on its side — in Explorer's small views and in the viewer's own first
  frame. The same was true of a turn baked into a JPEG (`Ctrl+M`) since
  v0.32, and both now go through one function, `metadata::set_upright`. An
  upright photograph keeps its thumbnail, which is what makes its first frame
  fast. Writing a new, turned thumbnail is an encode of its own and was not
  taken.
- An animation converts to its first frame, and says so.
- `tests/live_console.rs` runs the built binary over files on disk;
  `tests/docs_consistency.rs` holds the command-line page to `--help`.
