# 0001 — nitid owns the swapchain; egui renders widgets only

Date: 2026-08-09
Status: accepted

## Context

nitid must reach HDR output on Windows and apply ICC color transforms with full control over encoding. Both requirements land on the same resource: the presentation surface.

`wgpu` 30 exposes eight surface color spaces. On Windows the constraint is specific — DirectX 12 has no encoded-extended-sRGB swapchain color space and no HLG. The only HDR path is `Bt2100Pq` on the `Rgb10a2Unorm` format, selected by querying `SurfaceCapabilities` and scaling highlights through `DisplayHdrInfo::tone_map_headroom`.

A GUI framework that owns the event loop generally also configures the surface. `eframe` does. Working image viewers exist on `eframe` (FastView, SimpleImageViewer), which proves the widget layer is not the bottleneck — but they cannot express the surface configuration HDR requires without patching the framework.

Writing every widget by hand — buttons, checkboxes, combo boxes, a settings dialog, a text field with caret and selection, IME, accessibility — is several weeks of work that contributes nothing to the product's differentiator.

## Decision

nitid creates its own `winit` window and configures its own `wgpu` surface. The image is rendered by our code; ICC transforms are applied in the shader.

`egui` is integrated through `egui-wgpu` for widgets only — toolbar, settings, dialogs, thumbnail strip. This is a supported integration path, not a workaround.

Redraws are driven by events, not by a continuous loop: a static image costs no GPU time.

## Consequences

Positive:

- HDR is reachable: we choose the surface format and color space directly.
- ICC transforms happen on the GPU per frame instead of on the CPU at decode time — faster and lossless.
- Startup stays minimal — only a window and a device to initialize.
- The boring parts of the UI come from a mature library.

Negative:

- Roughly 200–300 lines of integration code between our renderer and the egui pass.
- Two mental models of rendering coexist in the codebase.
- Widgets look like egui. If that becomes unacceptable, replacing them is a contained change — the image path does not depend on it.

Rejected alternatives:

- **Pure `winit` + `wgpu`, hand-written widgets** — maximum control, but three to five extra weeks for UI that users will not credit us for.
- **Pure `eframe`** — fastest to a working viewer, but HDR requires patching the framework's surface configuration.
- **Slint** — the royalty-free license does cover proprietary desktop use at no cost, contrary to an earlier assumption; it was rejected because embedding a custom GPU-rendered surface is the least-travelled path of the candidates, and because a third-party badge in the About box conflicts with the product line's own identity.
