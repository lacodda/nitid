# 19. The histogram counts the file's values, not the display's

Date: 2026-08-31

## Status

Accepted.

## Context

The histogram (`H`) has to say what tones a picture is made of. There are two
defensible places to measure that, and they give different answers on exactly
the files a histogram matters for.

**After the colour transform** is "what is on the screen". The curve then
matches what the eye sees in the window, which sounds like the honest answer
for a viewer whose whole premise is honest colour.

**Before it** is "what the file records" — the values as the decoder produced
them, ahead of the ICC conversion the shader applies per frame.

The two diverge whenever the transform is not the identity: a Display P3
photograph on an sRGB monitor, a 16-bit file, anything HDR. And the divergence
is not cosmetic. Measured after the transform, the same photograph's histogram
moves when the window is dragged to another monitor, and it reports clipping
that belongs to the display's gamut rather than to the picture: a highlight
well inside the file's range shows as blown because *this* screen cannot reach
it.

nitid's colour path also makes the second measurement expensive. The transform
lives in the shader, on purpose (ADR 0005 and the colour module): the decoded
pixels stay as the file stored them, which is what makes a change of display
profile cost a redraw rather than a re-decode. Measuring after it would mean
either reading the frame back off the GPU or duplicating the whole transform on
the CPU — a second implementation of the thing the viewer is most careful
about, kept in step by hand.

## Decision

The histogram counts the values in the file, before the colour transform.
Decided by the owner, 2026-08-31.

Consequences that follow from it:

- **The reading does not change with the display.** The same file gives the
  same histogram on any monitor, in or out of HDR. What the panel reports is a
  fact about the picture.
- **Clipping is the file's clipping.** A blown highlight in the histogram is a
  highlight the camera blew, which is the one a photographer can still do
  something about.
- **Luminance is weighted on the stored values, not on linear light.** Rec. 709
  weights over the encoded numbers, so a mid-grey lands mid-axis where a
  photographer expects to find it. Weighting light instead would push every
  ordinary photograph's curve into the shadows and make a correct exposure read
  as underexposed — technically defensible and useless to the person reading it.
- **The count stays off the path to the first pixel.** It is arithmetic over
  the pixels the loader already holds, so it needs no GPU work and no second
  decode. It runs only when the panel is open, on a worker thread, and a large
  picture is sampled rather than counted whole.

## What followed from it

The same question came back in v0.21.0, twice, and the answer here settled both
without another decision:

- **The clipping zebra** marks what the *file* clipped. A highlight outside
  this monitor's gamut is not a highlight the camera lost, and hatching it
  would tell the photographer to fix something that is not wrong with the
  picture. It also keeps the two tools agreeing: a peak against the right-hand
  end of the histogram and a red hatch over the sky are the same fact.
- **The eyedropper** reports both — the file's numbers and what they become on
  this display — because "what colour is this" means the first to someone
  matching a brand colour and the second to someone matching what they see. But
  the value it *copies* is the file's, for the reason above: it is the one that
  is still true on another monitor.

The passport (`K`) exists because of this decision rather than despite it: once
the tools report the file rather than the screen, the conversion between them
becomes the thing a person needs explained.

## What this is not

It is not a claim that the on-screen measurement is worthless. A soft-proofing
view — "show me what this monitor will actually do to the picture" — is a real
feature, and if it is ever built it is a second reading beside this one, clearly
labelled, not a change to what `H` reports.
