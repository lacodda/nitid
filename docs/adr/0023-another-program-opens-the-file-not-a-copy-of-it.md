# 23. Another program opens the file, not a copy of it

Date: 2026-09-10

## Status

Accepted.

## Context

Looking at a photograph and wanting to change it is one motion, and until now
it went through Explorer: find the folder the viewer is already showing, find
the file the viewer already has open, right-click, "Open with". The viewer
knew both and offered neither.

Three questions had to be answered before any of it could be written: which
program opens the file, which keys say so, and what the viewer does while the
program is running.

## Decision

**`E` asks Windows, and the setting only overrides it.** The shell's `edit`
verb names the program already registered to edit that kind of file — the one
the context menu's own "Edit" would start. An empty setting therefore means
"ask the shell", not "unset", and the key works on a viewer nobody has
configured. The verb is `edit` rather than `open`, because `open` is nitid
itself once the viewer is the default for a format, and a key that reopens the
picture where it already is does nothing anyone asked for.

The alternative — `E` as a tenth assignable slot, silent until filled in —
was rejected as a setting standing in for a choice. Someone who wants a
different editor names one; someone who does not gets the one their system
already uses.

The two kinds of "nothing is set" are therefore **not** the same, and that is
the sharpest edge in this stage: an unset editor asks Windows, an unset digit
key says it has nothing on it. Swapping them compiles and would break the key
that matters most on a fresh installation, so the decision is held by a test
over a free function rather than left inside a method that needs a window.

**The digit keys take `Alt`.** `Ctrl` and the digits sort into folders and have
since v0.27.0, and `Ctrl+Shift` copies there; a feature added later does not
take a gesture that is already spoken for. `Alt` was unused in the viewer
entirely. The digits are read from the **physical** key, as sorting is —
`Alt+1` no more delivers the character "1" on every layout than `Ctrl+Shift+1`
does.

**Nothing waits for the program to finish** — `spawn`, never `status`. The
event loop is the thread that draws, and a photograph frozen behind a modal
image editor is a viewer that has stopped being one. The file is not reloaded
afterwards either: an edit takes as long as a person takes, and guessing when
that is finished would be worse than the `R` that reloads on request.

## Consequences

The viewer starts processes it does not own and does not supervise. That is
the honest description, and it is why the programs are named by the person
rather than discovered: this is not a plugin mechanism and must not grow into
one.

A file type with no `edit` verb registered — most raw formats, some vector
ones — makes `E` fail. The shell says so and the viewer repeats it, rather
than falling back to `open` and appearing to have done something.

`run` is not Windows-specific and so has one implementation for both builds;
only the shell verb needs a stand-in where there is no shell. Starting a
program is `Command::spawn` everywhere.
