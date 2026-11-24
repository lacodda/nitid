# 49. Explorer's thumbnails come from a thin DLL

Date: 2026-11-23

## Status

Accepted.

## Context

A folder of JPEG XL or HEIC is a folder of identical icons in Explorer unless
the machine has a codec for it, and the codecs come from the Store, one of them
for money. nitid opens all of them.

Explorer takes a thumbnail from a DLL it loads, which is code of ours running
for every file in every folder anybody looks at, on files nobody chose to open.
ADR 0042 refused a DLL for a menu entry. Here there is no other way in, so the
question is how little of it there can be.

Measured on 2026-11-23, on Windows 11 build 26300, by asking the shell for each:

| | Windows' handler is registered | Windows draws it |
| --- | --- | --- |
| AVIF, WebP | yes | yes |
| JPEG XL, HEIC | yes | no |
| SVG | no | no |

So a registered handler says nothing: Windows lists its own for formats the
machine has no codec for.

## Decision

**The DLL reads no picture.** It is handed a stream, starts the `nitid.exe`
beside it, passes the bytes on and makes a bitmap of the answer. `nitid.exe`
sets up the sandbox and hands the bytes over unread - it does not even work out
the format - and the decoder in the sandbox does the rest: the camera's own
thumbnail if it is large enough, the whole picture otherwise, made small, turned
and put in sRGB there. Every format goes through it, including those the viewer
decodes in its own process: the viewer opens what a person chose, and a
thumbnail is made of whatever is in the folder. There is no second path. With no
decoder there is no thumbnail.

**A stream, and not a path.** The plan was `nitid.exe --thumb FILE`. A handler
initialised with a path has to ask Explorer to load it into its own process; one
initialised with a stream runs in a host of its own, and a stream has no path to
pass on. What is kept is the point of the plan, that the sandbox is the one
place a stranger's file is read; what changes is that the bytes arrive on
standard input. It also covers what has no path: a picture inside a zip, one
still in the cloud.

**The DLL travels inside the executables.** An installed copy takes
`nitid.exe` and `nitidw.exe` out of the release that replaces it, by name, and
nothing else (ADR 0043). A third file in the zip would reach nobody who already
has nitid, and one an install required would leave every one of those copies
unable to update. So the build script compiles the DLL, the program carries it,
and an install writes it out. It is compiled with `rustc` from two source files
and no dependencies, with COM's four tables written by hand: a cargo package of
its own cannot be embedded by a stable cargo, and the `windows` crate would be a
second build of a large crate for four interfaces.

**Only where nobody else draws the format.** For JPEG XL, HEIC, AVIF and SVG,
at every install and every update: an extension another program has a handler
for is left to it, and a format Windows can decode is left to Windows. Windows
is asked by being given a small picture to decode, since the registry does not
know. A handler that names a program no longer there counts as nobody's. The
rest of what nitid opens, every Windows this runs on draws by itself.

Not always nitid's, which would have been simpler and the same on every machine:
it would have taken away something that works.

**`--decode-stdin` comes first on the command line.** Someone who installs an
older release over this one keeps the DLL. A version that has not heard of
thumbnails takes an argument it does not know for a file to open, and would open
a window for every file in the folder; every version there has been answers
`--decode-stdin` by saying what is wrong with its input and going. The reply is
versioned for the same reason, in the other direction: a DLL an update replaced
stays loaded until its host goes, and asks the new program.

**The codecs are asked last.** To learn what Windows decodes, it has to decode,
which loads the machine's codecs into the process doing the install. Everything
else an install or an update does is done by then.

## Consequences

Three processes a thumbnail: Explorer's host, `nitid.exe` and the decoder, two of
them started for it. Explorer keeps what it is given, so that is paid once a file,
and the chain runs below normal priority, so a folder being drawn gives way to a
picture being opened. The installed v0.45.0 on the development machine, program
and sandbox together: 0.3 to 0.5 s for a two-megapixel JPEG XL or HEIC and for an
SVG of shapes, 1.3 s for a twelve-megapixel HEIC with no thumbnail of its own, and
1.4 s for an SVG with text, a second of which is the fonts, found again by every
decoder since each is a process of its own.

A thumbnail has fifteen seconds in the decoder and twenty-five in all, after
which it is an icon. A file above 512 MB has none, which is the decoder's own
limit.

On a machine that gains a codec, Windows' own drawing comes back at the next
update rather than at once; on one that loses a codec, nitid's comes the same
way.

`tests/live_thumbs.rs` loads the DLL the way a host does and holds it to COM's
rules, to a bitmap of the right picture for each format, to a decoder that hangs,
and to the sandbox being the only reader; and it holds the registration in a
corner of the registry of its own. `tools/check-thumbnails.ps1` asks Windows
itself, on every push in CI and on the clean machine a release is rehearsed on.

Both run it as an ordinary account made for it
(`.github/scripts/thumbnails-as-a-person.ps1`). The registration is one person's,
in their own hive, and COM does not read that in an elevated process: on a runner,
which is elevated, the shell finds the handler listed and answers that its class
is not registered. The same is true of any program run as administrator - its
*Open* box shows icons where Explorer shows pictures. Registering for the whole
machine would need an administrator at every install, which nitid does not ask for.
