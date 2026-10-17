# 38. A key from anywhere in Windows is held by a thread of its own

Date: 2026-10-17

## Status

Accepted.

## Context

2.x is a screenshot tool, and a screenshot tool starts from a key pressed in
another program. v0.39 takes that key early, on something that cannot do
harm: `Win+Alt+N` brings the viewer forward with the newest picture in the
folders on the start screen — the screenshot just taken, the frame a generator
just wrote.

The plan said a resident process already existed to hold it. It did not: v0.16
gave the viewer one window, elected by a named pipe (ADR 0016), and that window
is a process that ends when it is closed. Nothing of nitid runs while no
window is open.

## Decision

**The key is held while a viewer is open, in any state — minimised, behind
other windows — and not after.** Staying behind once the window has closed is
a resident process, with a tray, a way to be told to go and a start at logon:
the capture tool needs all of that and will introduce it, and a resident built
here for one shortcut would be built twice.

**`RegisterHotKey` on a thread of its own, with its own message loop.**
Registered with no window, the key's `WM_HOTKEY` is posted to that thread,
which wakes the event loop through the same proxy a finished decode uses —
nothing polls. The window's own loop is winit's, and reaching into it
(`with_msg_hook`) would tie the key to how winit dispatches thread messages; a
thread that owns its registration is also exactly what a resident process will
need, having no window to hand one to. The thread makes its message queue
before registering — a `WM_QUIT` posted to a thread with no queue is lost, and
the listener would never stop — and lets go of the key on its way out.

**`Win+Alt+N` by default, and a setting.** The line's rule is one right
behaviour rather than a setting, and a key taken from the whole desktop is the
exception it allows: it collides with whatever else on that machine took it
first, and only the person at the machine knows which program should have it.
Not `Ctrl+Alt` and a letter, the combination most programs reach for: on
European layouts `Ctrl+Alt` is `AltGr`, and `Ctrl+Alt+N` taken globally is the
Polish ń gone from every program. A combination with no `Win`, `Ctrl` or `Alt`
is refused unless nobody types with its key — F13 to F24, `Pause`,
`Print Screen`. `MOD_NOREPEAT`, so a held key is one press.

**A combination another program holds is said, not swallowed.** The
registration is answered before `listen` returns; a refusal is a message on
screen and a line under the setting. A key that silently does nothing is a key
that gets pressed harder.

**"The newest picture" is the one most lately written, across the folders on
the start screen** — not the one most lately looked at, which is already on
screen. The folders are listed on a worker: a network share can take seconds
to answer, and the window has to keep answering meanwhile.

## Consequences

`hotkey::tests` register a combination nobody holds (every modifier and F22 or
F23) and hold two properties a mutation would break: a press posted to the
listening thread reaches the code that answers it, and a combination already
held is refused as held until it is let go of. A test cannot press a key that
belongs to the whole desktop it runs on; the live run did, with `SendInput`,
and the newest picture came up in front of a full-screen program.

A viewer minimised to the taskbar was restored by the key in the live run but
not brought in front; one that was only behind another window was. Why the
restore loses the foreground is not measured yet — the same raise serves a
file handed over from Explorer (ADR 0016) — and it is raised as a wish, with
what was seen, for a session where the machine is free to have its focus
taken.
