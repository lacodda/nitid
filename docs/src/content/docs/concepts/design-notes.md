---
title: Design notes
description: The two decisions that shape everything else in nitid.
---


Two decisions shape everything else:

**nitid owns its swapchain.** HDR output needs a surface format and color space chosen deliberately — `ExtendedSrgbLinear` on `Rgba16Float`, and re-chosen while the viewer runs as the display changes. A GUI framework that configures the surface for you closes that door, so the window and renderer are ours; `egui` is used for widgets only.

**Untrusted input is isolated, and slow input is interruptible.** An image decoder parses hostile data by definition — pictures arrive from the internet. Every decoder nitid ships is Rust, so a malformed file causes a panic or an error rather than code execution. The separate low-integrity process that was built for memory safety earns its keep for a different reason: a thread cannot be stopped and a process can, so a decode is abandoned when you navigate away and killed when it wedges.

Architecture decisions are recorded in [`docs/adr/`](https://github.com/lacodda/nitid/blob/main/docs/adr/).
