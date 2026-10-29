# 41. The space bar in Explorer is heard by a resident

Date: 2026-10-29

## Status

Accepted. Builds the resident ADR 0038 put off.

## Context

v0.41 brings the glance macOS calls Quick Look to Explorer: a picture
selected there, the space bar, and it is on screen; the space bar again, and
it is gone. The plan said a resident process already existed to hear the key.
It did not — ADR 0038 says so, and puts the resident off until a second thing
needed one, "with a tray, a way to be told to go and a start at logon".

This is the second thing. A key pressed in Explorer has to be heard while no
viewer is open, and a viewer is a process that ends with its window
(ADR 0016).

## Decision

**A resident: `nitidw.exe --resident`.** Started at sign-in by an entry under
`HKCU\...\CurrentVersion\Run` that `nitid install` writes, and by the install
itself. It holds no visible window, no graphics device and no picture. One per
session, by a `Local\` mutex: a second one started beside it leaves at once.
It is told to go by `WM_CLOSE` to its hidden window — `nitid install` does
that before replacing the executable it runs from, then starts the new one;
`nitid uninstall` does it before taking anything away.

**An icon in the notification area**, with *Open nitid* and *Quit*. It is how
someone who did not ask for anything in the background sees that something
is, and makes it go. Whether it starts at sign-in is not a setting of ours:
Windows lists the entry in *Settings → Apps → Startup*, with a switch, and
that is where a person looks for it.

**The key is read by a low-level keyboard hook on a thread of its own, and
never swallowed.** Explorer keeps its space bar, which on a selected file does
nothing. Swallowing it would mean asking Explorer what is selected inside the
hook, and a hook that waits on another process is one Windows removes without
a word once it has been slow a few times. So the hook decides only what the
keys decide — a bare space, pressed rather than repeated, not within a second
of a typed character (Explorer's list finds a file by its name as it is
typed, and "my photo" has a space in it) — and posts that to the resident's
window. `Ctrl+Space` and `Shift+Space` are Explorer's.

**The selection is asked of Explorer, not read off the screen.** The window in
front must be a folder window (`CabinetWClass`) or the desktop (`Progman`,
`WorkerW`), and the keyboard must be in its list of files — a `DirectUIHWND`
or `SysListView32` inside `SHELLDLL_DefView`, which leaves out a rename, the
address bar, the search box and the tree of folders. Then `ShellWindows` gives
the folder window's browser — the tab on top, in Windows 11, matched by its
`ShellTabWindowClass` — or the desktop's, and its `IFolderView` gives the
selection as shell items with paths. Items with no path on disk are left out.

**What a look shows is what a double-click would.** One folder is walked from
its first picture (ADR 0042); otherwise the pictures selected — one opens its
folder, several walk themselves. A selection with no picture in it is not a
look.

**Explorer starts the viewer for a look, as it does for a double-click.** Only
the window in front may bring a window forward on its own say, and a program
it starts inherits that right; a program the resident starts — never in front
— opens behind Explorer with its taskbar button flashing. On the development
machine `ForegroundLockTimeout` is at its maximum, and measured there neither
`SetForegroundWindow` after winit's press of Alt nor after `AttachThreadInput`
brings a window forward. So the resident asks the window in front to start
`nitidw.exe --peek` with the paths, through the shell object behind its view
(`GetItemObject(SVGIO_BACKGROUND)` as `IDispatch`, then
`IShellFolderViewDual::Application` → `IShellDispatch2::ShellExecute`), and
Explorer starts it as its own. That process is then exactly a double-click's:
with no window open it becomes the window; with one open it hands the paths
over down the instance pipe — whose message now says what is asked, an open or
a look (protocol version 2) — and hands the foreground right on with them
(ADR 0016). A selection too long for a command line, or an Explorer that will
not start it, is handed over by the resident itself, shown if behind.

**The space bar ends a look, and how depends on how it began.** A window the
look started closes. A window that was already open goes back behind the
window that asked, keeping what it now shows. A window handed a file any other
way — a double-click, "Open with" — is the person's from then on, and the
space bar walks it again. A slideshow and a comparison keep the space bar, as
they keep `Esc`: a mode answers its keys first.

**The global key stays with the viewer for now.** Moving `Win+Alt+N` into the
resident would change when it works — with no window open, too — which is the
capture tool's step to take along with its own keys, not a side effect of this
one.

## Consequences

`tests/live_shell.rs` runs the real binaries: a look the space bar started
ends with its window, a look at a window that was open leaves it open and
stepped nowhere, a second resident leaves, and one told to go goes. Each was
broken on purpose and the gate went red. Explorer itself cannot be driven by
the gate — its space bar takes the foreground from whoever is at the machine
— so `resident::explorer` has two live tests, run by hand. One opens a folder
window, selects a file through the automation and reads it back, and has
Explorer start a program whose arguments arrive whole; it needs no foreground.
The other brings the window in front with a click on its title bar and asks it
what the resident asks; it needs an unlocked screen, because nothing short of
input gets past the foreground lock and a locked screen takes none.
`explorer /select` was not usable to set a selection up: on Windows 11 it opens
the folder with the file focused and, measured, nothing selected.

A look from a cold start pays a cold start, about a third of a second; with a
window open it is a hand-over. Only Explorer and the desktop are heard: other
file managers, and the Open and Save dialogs, are lists of files Explorer's
automation does not reach.
