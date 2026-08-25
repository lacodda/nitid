# 0013 — HDR output goes through scRGB, and follows the display

Date: 2026-08-24
Status: accepted

Amends: [0001](0001-own-the-swapchain.md), whose claim that `Bt2100Pq` is the only HDR path on DX12 is corrected below.

## Context

ADR 0001 committed nitid to configuring its own surface so that HDR would be reachable, and named the configuration: "DirectX 12 has no encoded-extended-sRGB swapchain color space and no HLG. The only HDR path is `Bt2100Pq` on the `Rgb10a2Unorm` format." That sentence was written from documentation, in the stage that chose the architecture. Building the stage began by measuring it.

DX12 on the machine nitid is developed against (Intel Iris Xe, driver 32.0.101.5768) reports:

```
Bgra8UnormSrgb: SRGB
Rgba8UnormSrgb: SRGB
Bgra8Unorm:     SRGB
Rgba8Unorm:     SRGB
Rgb10a2Unorm:   SRGB | BT2100_PQ
Rgba16Float:    EXTENDED_SRGB_LINEAR
```

Two HDR pairs, not one. `Bt2100Pq` on `Rgb10a2Unorm` is there as ADR 0001 said, and so is `ExtendedSrgbLinear` on `Rgba16Float` — scRGB. ADR 0001 was right that DX12 has no *encoded* extended-sRGB space (`ExtendedSrgb`, the gamma-carrying sibling) and no HLG; it was wrong to conclude from that that PQ was all that remained. The linear extended-range space is a different entry, and it is present.

The two differ in what the shader must hand over:

- **PQ** wants values already in the BT.2020 gamut and already encoded through SMPTE ST 2084, where `0.0..=1.0` spans 0 to 10,000 nits absolutely. That is a second primaries matrix, a transfer curve, and a decision about where SDR white sits in nits, all downstream of the colour management nitid already does.
- **scRGB** wants linear light in BT.709 primaries, where `1.0` is SDR reference white and values above it drive the display's headroom. That is precisely what nitid's fragment shader already computes on its way to the sRGB encoder — the ICC transform decodes to linear light and moves between primaries, and the final step encodes for the surface.

The second fact the measurement produced is about the display rather than the API. The author's panel is HDR-capable (617 nits peak, Display P3 primaries) and the Windows HDR toggle was **off**: `tone_map_headroom` reported `1.0`, `bits_per_color` reported 8. A viewer that decides once at startup whether it is an HDR application would be wrong for whichever state the user changes to next — and the toggle is a switch people flick, not a property of the machine.

## Decision

**nitid outputs HDR through `ExtendedSrgbLinear` on `Rgba16Float`.**

The shader's existing output — linear light — is written to the surface unencoded and unclamped above 1.0. The floor is still held at zero, because negative light is not a colour. No tone mapping is applied and no highlights are scaled: an SDR image contains nothing above SDR white, so on an HDR surface it lands exactly where it lands on an SDR one, which is the correct result and is asserted as such by drawing both and comparing.

`Bt2100Pq` is not configured. It would mean encoding for a colour space to have the compositor decode it again, in exchange for nothing the viewer can currently show — the decoders upstream deliver 8 bits per channel (see the wider-buffer stage that follows this one).

**The choice is re-made as the display changes.** `hdr::choose` takes surface capabilities and the display's live `tone_map_headroom`, and both halves must agree: HDR is configured only when the surface supports the pair *and* the display reports headroom above `1.05`. Asking the display costs about 140 µs — small against a decode, far too much for a frame that would otherwise be free — so it is asked when the window moves, resizes, or regains focus, rather than every redraw. Turning HDR on in Windows re-sets the display mode and moves every window; coming back to the viewer afterwards catches the rest. The idle loop stays asleep either way, which is the promise ADR 0012 made.

A headroom the platform will not report is treated as standard range. Guessing HDR on a display that would not say puts a picture on screen nobody asked for.

## Consequences

Positive:

- The HDR path is the shader's own arithmetic with one step removed rather than a second colour pipeline beside it. The extended-range branch is four lines.
- 16 bits per channel across the whole frame buffer, which is more headroom against banding than the 10-bit alternative, in a viewer whose subject is images.
- Turning HDR on or off in Windows is followed while the viewer is open. No restart, and the startup report states which surface is up, so the answer is readable rather than judged by eye.
- The correction to ADR 0001 is a measurement rather than a revision of taste: the probe that produced the capability table is reproducible on any machine.

Negative:

- A 16-bit frame buffer is twice the bytes of a 10-bit one. On an HDR display that is the cost of the format; on an SDR display it is not paid, because the surface stays 8-bit sRGB there.
- Hardware that offers PQ but not extended linear gets SDR from nitid. None has been met; if one appears, `hdr::choose` is where the second pair would go, and the shader would need the PQ encoder written then.
- HDR content itself — a 10-bit AVIF, a HEIC with a PQ transfer — still arrives narrowed to 8 bits, because every decoder path hands over RGBA8. The surface can now carry more light than the pixels contain. Widening the buffer through the decoders, the sandbox protocol, and the texture is the stage that follows.

Rejected alternatives:

- **`Bt2100Pq` on `Rgb10a2Unorm`**, as ADR 0001 named it. Rejected on the grounds above: more shader, no more picture, and a conversion into a gamut that would have to be undone by the compositor for the same panel.
- **Configuring HDR whenever the surface supports it**, ignoring the display's live state. Simpler — no follow — but it spends a wider frame buffer permanently on a machine whose HDR toggle is off, which is the state this one is in most of the time.
- **Deciding once at startup.** Half the code of following, and wrong exactly when a person changes the setting to look at a photograph, which is when they are most likely to change it.
