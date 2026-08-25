# 0014 — PQ reference white lands on SDR white

Date: 2026-08-25
Status: accepted

## Context

The wider buffer (v0.14.0) lets 10- and 12-bit AVIF and HEIC open, and the common reason such files exist is HDR10: BT.2020 primaries with the SMPTE ST 2084 (PQ) transfer. nitid's colour pipeline turns a file's CICP description into a profile whose tone curves the shader samples into linear light.

PQ is unlike every other curve the viewer meets: it is **absolute**. `moxcms` normalises it so that code 1.0 decodes to 1.0 at ten thousand nits — measured, not assumed: the decoded curve puts the PQ code for 203 nits at 0.0203. HDR reference white (203 nits, BT.2408) therefore lands at two percent of the pipeline's white, and an HDR10 photograph would reach the screen nearly black — on the SDR surface *and* the HDR one, whose 1.0 is SDR white with headroom above it, not ten thousand nits.

Every other curve in the pipeline — sRGB, gamma, ICC LUTs, and HLG, which is scene-relative — already treats 1.0 as its white. PQ alone needs placing.

## Decision

**A PQ profile's linear light is scaled by 10000/203, folding BT.2408 reference white onto 1.0.** The scale is multiplied into the conversion matrix in `ColorTransform::new`, so the shader gains no extra step and no uniform: the same matrix multiply that moves primaries also sets the level.

What each surface then does follows from v0.13.0's arrangement without further code:

- The **SDR surface** clamps at 1.0: content up to reference white shows at its intended level, highlights above it clip to white. Clipping is the simplest sensible rendering intent for a viewer, and the same one out-of-gamut colours already get.
- The **HDR surface** carries values above 1.0 into the display's headroom: highlights up to the panel's limit are shown, and past that the display itself clips.

No tone mapping curve is applied in either direction. Detection is by the profile's stored CICP transfer characteristic (`Smpte2084`), which `moxcms` records when the profile is built from CICP — the path every AVIF and HEIC colour description takes.

## Consequences

Positive:

- HDR10 files open at a sensible brightness everywhere, and actually use the headroom on an HDR display.
- Zero cost: the scale rides in a matrix that was already there.
- The check is end to end in a unit test: PQ code for 203 nits in, 1.0 out, through the sampled curve and the matrix.

Negative:

- Highlights above the display's reach are clipped, not compressed. A proper tone mapper (rolling highlights off toward `tone_map_headroom`) would preserve their texture; it is more code and more opinion than this stage needs, and the clip is honest about where the panel ends.
- HLG is left scene-relative, shown with 1.0 as its nominal peak — no OOTF is applied. HLG stills are rare; recorded here rather than guessed at.
- The 203-nit figure is BT.2408's convention, not something the file states. A mastered-for-1000-nits image whose grader put diffuse white elsewhere will sit brighter or darker than intended — the same compromise every viewer without dynamic metadata makes.
