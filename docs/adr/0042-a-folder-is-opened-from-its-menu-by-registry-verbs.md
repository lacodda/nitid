# 42. A folder is opened from its menu by registry verbs

Date: 2026-10-29

## Status

Accepted.

## Context

Until v0.41 the viewer opened files. Named a folder — on the command line, in a
drop — it read the folder's *parent* and showed whatever came first there,
because a folder is a path like a file and `Folder::open` lists the folder a
path sits in. v0.41 puts "Open with nitid" on a folder's menu in Explorer, so
the folder has to mean itself.

Explorer's menu takes an entry in two ways: a verb written into the registry,
or an `IExplorerCommand` from a DLL Explorer loads. Windows 11 shows the
second at the top of its menu — but only from a package with an identity,
which is a signed MSIX. Without one, it lands under *Show more options* with
the registry verbs.

## Decision

**A folder is a place to walk, from where its order begins.** The first
picture by name, or the newest or the largest when the settings sort that way;
the listing is followed like any other (ADR 0034). A folder with no picture in
it says so and, in a window that has nothing on screen yet, opens the start
screen rather than an empty canvas. Handed to a window already open, a folder
takes the window over — it is a place, not one more file in the walk that was
going on, which is what files handed over are (ADR 0016).

**Registry verbs, no DLL.** Under `HKCU\Software\Classes\Directory\shell\nitid`
on a folder (`%1`) and `Directory\Background\shell\nitid` on the empty space
inside an open one (`%V`, the folder being looked at), each running
`nitidw.exe` with the folder quoted. A DLL loaded into Explorer is code that
can take Explorer down with it, for an entry that would sit exactly where the
verbs sit unless the build were signed — which costs a certificate. The verb
on a folder offers itself only when one folder is selected
(`MultiSelectModel=Single`): the viewer walks one place at a time.

**`SHChangeNotify(SHCNE_ASSOCCHANGED)` after registering and after
removing**, so a folder window already open reads the menu and the file icons
again rather than keeping what it read before.

## Consequences

On Windows 11 the entry is under *Show more options* (`Shift+F10`), said on the
page that describes it. Reaching the top of the menu is a signed package away,
and a signing certificate is the owner's to buy or not.

`tests/live_shell.rs` opens a folder and reads the first picture's name in the
title, hands a folder to an open window and sees it taken over, and opens a
folder with no picture on the start screen.
