# 5. Untagged images pass through unconverted

Date: 2026-08-12

## Status

Accepted. Supersedes the untagged-image half of the colour rule introduced with
v0.3.0.

## Context

v0.3.0 gave nitid colour management: an image's ICC profile is read from the
file, the display's profile is read from Windows, and the conversion runs in the
shader. For a file *without* a profile, that release assumed sRGB and converted
from it. The reasoning was recorded at the time: an sRGB image sent unconverted
to a wide-gamut display comes out oversaturated, so assuming the convention and
converting seemed strictly more correct than doing nothing.

Daily use showed the opposite. On the author's OLED display — noticeably wider
than sRGB, with a red primary at X=0.58 against sRGB's 0.44 — untagged PNGs came
out visibly washed out next to the same files in other viewers. The matrix
explains it exactly:

```
sRGB -> display
  0.7335  0.2387  0.0255
  0.0330  0.9575  0.0103
  0.0172  0.0794  0.9045

neon green (0.10, 1.00, 0.20) -> (0.32, 0.96, 0.26)
cyan       (0.00, 0.80, 1.00) -> (0.22, 0.78, 0.97)
```

Three times the red mixed into a saturated green. The arithmetic is right; the
premise is not.

The premise fails because "untagged means sRGB" is a statement about what the
numbers *mean*, and it is being used to justify changing what they *are*. Every
other program on the machine — Windows itself, the shell preview, browsers,
the file manager's viewer — sends untagged pixels to the screen unchanged. An
untagged image therefore has an established appearance on this display, and it
is the appearance its author saw while making it. Converting makes nitid the
only program showing that file differently, which is the opposite of what a
viewer promising honest colour should do.

The oversaturation argument still holds for photographs, but it argues for
tagging files, not for nitid guessing on their behalf.

## Decision

A file that carries a colour profile is converted to the display profile, as
before. A file that carries none is passed through untouched — the identity
transform, no matrix, no curve.

The choice lives in `ColorTransform::for_image`, one function with one branch,
so the rule is stated once and can be tested directly.

## Consequences

- An untagged image looks the same in nitid as everywhere else on the machine,
  including the tool that produced it.
- Colour management still applies where a file actually states its colour space:
  Display P3 and Adobe RGB photographs are converted, which is the feature's
  real purpose.
- Formally, an untagged sRGB image on a wide display is now shown more saturated
  than the standard would have it. This is accepted: matching every other
  program on the platform matters more than being uniquely right in a way the
  user experiences as wrong.
- A per-image or global "treat untagged as sRGB" switch would restore the strict
  behaviour for anyone who wants it. Not built, because nobody has asked; the
  branch it would hang off is a single `match`.
- Guarded by `an_untagged_image_is_never_converted_even_on_a_wide_display` in
  `tests/color.rs`, which fails if the old behaviour returns. The earlier test
  only asserted that no profile was *found* — never that the file was then left
  alone, which is how the regression reached a release.
