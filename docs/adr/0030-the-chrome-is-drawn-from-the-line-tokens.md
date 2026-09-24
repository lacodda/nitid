# 30. The chrome is drawn from the line's tokens

Date: 2026-09-24

## Status

Accepted.

## Context

nitid is one of a line of products, and the others share a look through
dowel, a design system published as the `dowel-ui` package. Most of them are
web views and take its CSS directly. nitid cannot: it owns its swapchain so it
can present HDR (ADR 0001, ADR 0013) and apply ICC transforms in its own
shader, and a web view expresses neither. Moving to one was considered and
rejected for that reason — it would cost the product its first promise, not
only its startup time.

That left the chrome — the toolbar, the status line, the panels and dialogs —
drawn by egui with colours typed into `interface.rs` by hand: twenty-seven
`Color32` values, chosen to look right on the day and matching
nothing else in the line. Each dowel release would have had to be carried over
by reading five thousand lines for greys.

## Decision

The colours come from dowel's own files, not from a copy of their values.

- `assets/dowel/palette.json` is dowel's export of every colour of its theme
  resolved for nitid's accent, in both themes, for consumers that cannot
  evaluate CSS. `assets/dowel/tokens.json` is its scales — radius, type,
  motion. Both are copied verbatim from a published `dowel-ui` release; the
  palette names that release, and a CI job fetches it and compares both files
  byte for byte.
- `build.rs` turns them into Rust constants (`theme::dowel`). A missing theme,
  a colour in a form it does not know, a palette of another product or a
  dimension that is not in pixels stops the build rather than being guessed.
- `src/theme.rs` builds egui's visuals for both themes from those constants
  and hands egui both; egui follows the system theme through `egui-winit`.
  The window's theme is passed on the first frame so the chrome does not open
  dark and turn light.
- `interface.rs` names no colour by its numbers. A test reads its source and
  fails on any `Color32::`, `Rgba::`, `Hsva` or `ecolor::` outside comments
  and tests, and a second test shows it colours it must find, so the gate
  cannot go blind by matching nothing.

Some colours are deliberately not tokens, and they live in `theme.rs` under
names that say why: marks drawn on the photograph (black and white paired, so
they read on any picture), the histogram's channels (red, green and blue
because that is what they measure), a picture's own pixels, and the
translucency of a panel laid over the photograph, which dowel has no token for
yet.

The scene behind the photograph is not themed at all. It is the renderer's
neutral grey, because a tinted backdrop changes how the picture's colours are
judged.

## Consequences

- A dowel release reaches nitid as a copy of two files and a rebuild. What
  moved shows up as a diff of JSON, and the tests on the accent and the
  control radius name the change a person should look at before accepting it.
- The line's top bar — mark, name, trail, actions — is nitid's toolbar too,
  drawn with the same mark as the taskbar icon (read from the embedded
  `icon.ico`, not a second copy).
- There is no splash screen. The window stays invisible until the first frame
  of the photograph, which keeps the startup promise; the shared look is
  carried by the mark, the colours and the bar instead.
- The fonts are still egui's own. dowel's typeface is a web font; embedding it
  adds weight to a binary whose startup is measured, and that is a separate
  decision with its own measurement.
