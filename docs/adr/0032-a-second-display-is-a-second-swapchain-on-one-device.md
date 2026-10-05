# 32. A second display is a second swapchain on one device

Date: 2026-10-05

## Status

Accepted.

## Context

A slideshow is most often watched on a screen other than the one it is run
from: a television across the room, a projector, while the person who started
it stays at the laptop. The viewer had one window and one swapchain, and the
renderer held both the device and that swapchain as one thing.

Three ways to put the picture on another display were on the table:

- **Move the window there.** Full screen on the television, and the laptop is
  left with nothing — no status line, no way to see what comes next, and the
  keyboard talking to a window across the room.
- **A second device for the second window.** Every picture would be uploaded
  twice, and a sixty-megapixel photograph is the one thing this viewer is
  careful never to copy.
- **A second window, its own swapchain, the same device.** The pictures are
  already resident; only the surface is new.

The third is the only one that keeps the laptop a working window. It has two
consequences the renderer was not shaped for: the two surfaces may want
different encodings — the television in HDR, the laptop not — and every
picture's colour uniform says how to encode for one surface.

## Decision

**The renderer splits into what draws pictures and what shows them.**
`Pictures` holds the device, the queue, the resident pictures, the viewing
choices that apply to every screen (backdrop, zebra, thresholds) and one
render pipeline per target format, built when a screen first asks for it. A
`Swapchain` is a surface, its configuration and the output chosen for its
display. The `Renderer` holds `Pictures`, the main swapchain and, while the
other display is up, a second one. Calls that depend on a screen name it:
`Screen::Main` or `Screen::Second`.

**A picture's uniform is rewritten for the screen about to be drawn.** Rather
than one uniform per picture per screen, `Pictures` remembers which output the
uniforms are encoded for and rewrites them — eighty bytes each — when the next
frame is for a screen of another kind. A queued write lands before the submit
that follows it, so each screen's frame reads the encoding written for it. Two
screens of one kind never rewrite at all.

**The other display shows the same part of the picture at the same share of
its screen.** Its framing is worked out from the main window's on every frame
(`View::on_screen`), by content rather than by pixel scale — the rule a
comparison already uses between two pictures, here between two screens. A
zoom into a face at the laptop is the same face, filling the television the
way it fills the laptop. A comparison is laid out for the television's own
shape by the same function that lays it out at the laptop.

**It draws only when what it shows has changed.** What a frame of the other
display shows is a value — the panes, their framing, and a count of changes
to the pictures themselves — and a frame is drawn only when that value moves.
Re-encoding is not counted as a change, or the two screens would ask each
other for frames for ever. The check happens where the event loop settles,
not after the main window's frame: a minimised laptop window draws no frames,
and the television must go on.

**It is part of the main window, not a second program.** Full screen on its
display, no chrome, no pointer, no button on the taskbar, never the window
with the keyboard. A key pressed over it does what it does at the laptop, and
`Esc` there takes the picture off it rather than ending the viewer. When it
stops being another display — unplugged, so Windows moves it onto the laptop,
or the main window dragged onto it — it closes rather than cover the person's
own screen.

## Consequences

- `Pictures` needs no window, so the suite can finally hold one: the rewrite
  for a second screen, and the rule that it is not a change of picture, are
  tested by drawing one picture onto two offscreen targets in turn. Until now
  the colour uniform's rewrite was covered only by pressing `B` by hand.
- A screen that switches between HDR and SDR finds its pipeline already built
  the second time.
- Colour is converted for the profile of the primary display on both screens,
  as it already was for the main window on any display: the profile is read
  once, from the primary. Converting per display is its own piece of work.
- A vector picture is rasterised for the larger of the two screens, so an icon
  on a television is drawn for the television.
- `NITID_SECOND_SCREEN_HERE=1` lets the other display be a plain window on the
  same display, which is how the second swapchain is exercised on a machine
  with one.
