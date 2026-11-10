# 44. A picture stating code points is the sRGB it decodes to

Date: 2026-11-10

## Status

Accepted. Supersedes the part of [ADR 0007](0007-heic-decodes-in-rust.md) that
attaches no profile to a HEIC stating CICP code points.

## Context

A HEIC states its colour one of two ways: an embedded ICC profile, or three
CICP code points in an `nclx` box. With code points, `heif-oxide` resolves the
colour itself and hands over sRGB (ADR 0007). ADR 0007 then attached no profile
to those pixels, reasoning that "attaching the file's profile would convert an
already-converted image a second time".

The reasoning was right about the file's profile and wrong about the pixels. A
picture with no profile is untagged, and an untagged picture is shown
untouched (ADR 0005): its numbers go to the display as they are. On an sRGB
display that is the same as converting from sRGB. On a wide-gamut display it is
not — sRGB numbers shown untouched on a Display P3 panel are read as P3 and come
out oversaturated. Every HEIC stating code points was shown that way, on the
kind of display the product's colour promise is about.

The same mistake showed in what the viewer said. The colour passport and the
status line called such a file "untagged", "a file that does not say what its
colours mean", when it says a great deal, only in numbers.

## Decision

**The pixels carry the profile of what they are.** A HEIC stating code points
decodes to sRGB, so its pixels carry an sRGB profile, and the shader converts
them to the display once, exactly as it does an sRGB JPEG. A HEIC with an ICC
profile carries that profile, as before. A HEIC stating nothing is untagged, as
any file stating nothing is. `image_source::heic_pixels_profile` makes the one
decision, and the full decode, the thumbnail and the sandboxed decode all use
it, so the quick frame and the picture replacing it are the same colour.

**What the file stated is kept beside the profile, not folded into it.**
`LoadedImage::codes` holds the code points, read on the trusted side from the
bytes. The profile describes the pixels; the codes describe the file. Folding
them together would have meant an sRGB profile claiming P3 code points — a
profile that disagrees with itself, and that an export would then write into
another file.

**The passport and `nitid info` say the codes**, in the same words
(`color::codes_said`): `1/13/6: BT.709 primaries, sRGB curve`, decoded as sRGB.
`nitid info` keeps reporting no profile for such a file, because it has none of
its own.

## Consequences

- On a wide-gamut display, HEIC photographs stating code points change colour:
  they are shown as the sRGB they are rather than oversaturated. On an sRGB
  display nothing changes.
- A HEIC stating code points, saved or converted with its profile kept, now
  carries an sRGB profile in the new file. That is what its numbers are, and a
  program reading the new file converts it correctly instead of guessing.
- The limitation ADR 0007 records stands: a HEIC stating wide primaries or an
  HDR transfer is still folded into sRGB by the decoder, and the caveat still
  says so.
