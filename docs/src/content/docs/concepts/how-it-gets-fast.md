---
title: How it gets fast
description: The order of operations that puts a picture on screen before the full decode finishes.
---

Speed here is not a faster decoder — it is a different order of operations:

1. The embedded EXIF thumbnail is decoded in single-digit milliseconds and drawn immediately.
2. The full image decodes on a background thread and replaces it without a flicker.
3. Neighbouring files in the folder are prefetched, so arrow keys never wait.

Measured on a 24-megapixel JPEG, from process start:

```
window created at    11 ms
gpu ready at        118 ms
thumbnail up at     128 ms   <- the picture is on screen here
first pixels in     146 ms
```

The full decode of that same image takes about 120 ms and lands afterwards,
replacing the thumbnail in place. Most of what remains is the graphics driver
starting up, not work nitid controls — which is why the order of operations
matters more than decoder benchmarks.

Run with `NITID_STARTUP_REPORT=1` to get that breakdown for your own machine.
The numbers are held to a threshold by `tests/startup.rs`, so a change that
puts a full decode back on the startup path fails the build rather than
quietly costing a tenth of a second.
