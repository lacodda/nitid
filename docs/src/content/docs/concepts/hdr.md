---
title: HDR
description: Extended-range linear output that follows the display's own state.
---

On a display in HDR mode, nitid outputs extended-range linear light — scRGB,
`ExtendedSrgbLinear` on an `Rgba16Float` surface — so highlights above SDR
white drive the display's headroom instead of clipping at white. It is the
same shader either way: the colour transform already ends in linear light, and
an HDR surface simply takes it unencoded and unclamped. An SDR image therefore
looks identical on both surfaces, which is asserted by drawing it twice and
comparing the pixels rather than by eye.

The choice follows the display rather than being made once. Turn HDR on in
Windows with nitid open and the swapchain is reconfigured without a restart;
turn it off and it goes back. Turning it *on* announces itself to the window,
so that direction costs nothing; turning it *off* announces itself to nobody at
all, so while — and only while — nitid is on an HDR surface it asks the display
once a second, for the 140 microseconds that costs. On an SDR display nothing
polls and the event loop sleeps until you act, exactly as before.

`NITID_STARTUP_REPORT=1` states which surface is up and how much headroom the
display reports:

```
nitid: surface Rgba16Float ExtendedSrgbLinear, display headroom 7.71x
```

A screenshot of an HDR window is a standard-range image, so this line is the
one way to check the answer rather than judge it. See
[ADR 0013](https://github.com/lacodda/nitid/blob/main/docs/adr/0013-hdr-output-goes-through-scrgb.md).
