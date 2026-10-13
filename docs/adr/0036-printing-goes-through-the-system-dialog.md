# 36. Printing goes through the system's print dialog, as sRGB at 300 pixels to the inch

Date: 2026-10-13

## Status

Accepted.

## Context

v0.38 adds `Ctrl+P`. Windows offers three ways for a desktop program to print a
picture, and they differ in what the person sees and in what the program has to
draw:

- **The classic common dialog** (`PrintDlgEx`) hands back a device context and
  nothing else: no preview, the look of Windows 7, and every choice about the
  page — orientation, how the picture meets it — left to the program to ask
  separately or not at all.
- **The "Print Pictures" wizard** (`photowiz`, reached through its shell drop
  target) has layouts and a preview, but it prints a file: it reads the file
  with WIC, so it knows only the formats Windows decodes, takes the stored
  pixels rather than the turned and converted ones on screen, and is a legacy
  component reached through an undocumented class.
- **The modern print dialog** of `Windows.Graphics.Printing` — the one the
  Photos app opens — is reachable from a desktop window through
  `IPrintManagerInterop`. It has the preview, the printer, the paper, the
  orientation and the driver's own options, in the system's language, and lets
  the program add options of its own. It asks the program for a document
  source and draws whatever that source draws.

Separately: what colour the printer is given. A picture can be handed over in
its own numbers with its ICC profile — XPS can carry one — or converted to sRGB.

## Decision

**The modern dialog.** `print::dialog` registers for one `PrintTaskRequested`
on the window, opens the dialog, and answers with a document source: one COM
object implementing `IPrintDocumentSource`, `IPrintDocumentPageSource` and
`IPrintPreviewPageCollection`. The preview is drawn with Direct2D onto a DXGI
surface at the window's own DPI; the pages for the printer are drawn into a
Direct2D command list and handed to a Direct2D print control, which writes XPS
with the picture as a lossless image inside. The print system calls the source
from threads of its own; Direct2D's factory is the multithreaded one and what
the source holds is behind one lock. A Direct3D device is taken from hardware
where there is one and from WARP where there is not.

The page starts in the picture's orientation. nitid adds one option of its own,
**Picture: whole on the page / filling the page** — the same two ways a picture
meets a frame of another shape that the desktop has, one type (`fit::Fit`) for
both — kept for the session and not saved: the next start prints whole, the one
choice that never cuts anything off.

**sRGB.** The picture is converted the way an export with the colour baked in
is (ADR 0025): the shader's path, at the depth it was decoded at, with HDR
clipping where a standard-range screen clips it. Then it is turned upright,
laid on white, and sent at **300 pixels to the inch** of the paper it covers,
never more than it has. Only the part that shows is sent.

sRGB rather than the picture's own profile because sRGB is the one handoff every
driver path reads correctly. A profile in XPS is honoured by drivers that manage
colour, but a printer path that ignores it reads the numbers as sRGB, and a
wide-gamut photograph then prints visibly wrong with nothing to say so — a
wrong colour is worse than a narrower one.

## Consequences

- One `Ctrl+P` works for every format nitid opens, the pasted picture, a turn
  that was never saved, the frame of an animation that is up, and an SVG drawn
  sharp for the paper.
- The page can be made without the dialog and without a printer:
  `print::write_xps` writes the same page through the same print control into
  an XPS file, and `tests/live_print.rs` reads it back — where the picture is on
  the page, and its pixels, decoded by another decoder. It runs on a build
  machine with no printer installed.
- Colour beyond sRGB does not reach the paper, as it does not reach a
  standard-range screen. A print with the picture's own profile, for printers
  that manage colour, would be its own decision.
- The preview's surfaces are Direct3D 11 on a device of their own, beside the
  viewer's wgpu device; they live only while the dialog is up.
