# 43. A release installs itself, from the resident

Date: 2026-11-04

## Status

Accepted.

## Context

nitid is a program people use every day and never think about, so a version
that has to be fetched by hand is a version most of them never get. Updating
also has a property nothing else here has: **a copy already out there cannot be
corrected.** A mistake in the updater of one release is carried by every machine
that has it, and the release that would fix it cannot reach them.

The README promised "nothing that phones home". Asking GitHub for a newer release
is a request from the machine, so the promise had to change along with the
product, and what replaces it has to be exact about what is sent.

## Decision

**The resident asks, not the viewer.** It is always there, holds nothing, and has
nothing on screen to interrupt (ADR 0041). The viewer still starts without asking
anybody anything and still reaches no network, so nothing here can touch the
first pixel. It asks once a day, two minutes or more after sign-in, on a thread of
its own: an answer can take a minute and the space bar cannot wait for it. A copy
that is not the installed one does not ask, because it has nothing to replace.

**One small manifest beside every release.** `update.txt` holds `version`,
`archive`, `sha256` and `size`, written by the release workflow and read from
`releases/latest/download/update.txt`. Not the tag the `latest` redirect names,
which is what the install script reads, because the manifest can grow: a line a
copy does not know is passed over, so a later release can say more to later copies
without tripping earlier ones. It also brings the digest in the one request that
was being made anyway, and a draft is not served at that address, so a release is
still offered to nobody until it has been checked on a real machine and published.
The manifest can name only a file, and this program builds the address from it,
so the manifest cannot send anyone elsewhere.

**WinHTTP, the system's hashing, and a short zip reader over `flate2`.** No new
crate. WinHTTP brings TLS, checks the certificate against the machine's store,
follows the redirect to where GitHub keeps assets, and honours the machine's proxy;
a client crate would add a TLS stack to a viewer for one request a day. The zip
reader is about a hundred lines and reads only bytes that have already matched the
digest, and a file's name is used only for its last component.

**What is fetched is believed in a fixed order.** The size and digest the manifest
gave; each file's CRC; the program itself, run from where it was unpacked, saying
it is the version promised. Nothing is run before the digest matches. Older or equal
to what is running is not an update, whatever the manifest says, so withdrawing a
release sends nobody backwards.

**No signature of our own.** A key held by the same CI that builds the release adds
little to TLS and the account that publishes it, and costs a key to look after for
ever. Real authenticity is Authenticode, which is a certificate someone has to buy.
Until then what an update trusts is what the first install trusted, and the docs
say so.

**The new version installs itself.** The old copy unpacks it and starts its
`nitidw.exe --apply-update`, and from there it is the new version's code running
the same steps as `nitid install`: stop the resident, replace the files, write the
registration, start the resident. So the only things old copies depend on are
three, and they do not change: the manifest's address and keys, both executables
being in the zip, and that argument. The resident's window class is held to the
same promise, since it is how a new version finds an old one's resident.

**An update changes the program and leaves the person's choices alone.** An install
is someone asking, so it puts everything back. An update runs with nobody asking, so
it writes no shortcut, no `PATH` entry and no sign-in entry, and a person who took one
away has it stay gone. It restarts the resident only if one was running.

**Associations survive because nothing touches them.** The default program for a type
lives in `FileExts\.ext\UserChoiceLatest`, signed by Windows against the ProgID's
name. The name never changes, and nothing here writes under that key. `UserChoice`
beside it is the older key and can be stale: on the development machine it still names
the program chosen before nitid existed.

Measured there on 2026-11-04: defaults chosen on 2026-08-10, 08-14 and 09-15 unchanged
through some forty installs over themselves, the cumulative Windows updates of October
and November, and the move from build 26200 to 26300. A full upgrade with a migration
has not happened in nitid's lifetime, so that one case is unobserved. Such an upgrade is
one Windows may reset defaults in by design, and no program can set them back.

**Both executables or neither.** An install used to rename each file away and copy the
new one in, so a full disk could leave nothing where the association points, or
half of one version beside half of another. Nobody is watching an update to run the
script again. Now everything that can fail for ordinary reasons fails while copying
beside the targets, and the rest are renames that can be walked back. Once in place
the copy must match and must answer `--version` from there, since a machine's policy
or an antivirus decides by where a program is; otherwise the old pair goes back.

**A window left open is allowed for, three ways.** It holds the file an update
retired for as long as it stays open, across as many updates as arrive, so retired
names are found free rather than assumed free. Handing it a picture from a new
version whose protocol has moved on would be refused and the sender would never
know, so the pipe's name carries the protocol's version, and the new version opens a
window of its own. And it starts the new `nitid.exe` as its decoder, because that is
the name it knows; if their protocol has moved on, the decoder answers rather than
dying, and the window says to close it rather than blaming the file.

**A switch, against the line's rule against settings.** *Settings → Updates*, or
`updates = off`. This is the one thing nitid does that reaches the network, and on a
machine that must not, no other behaviour will do. On by default, because a promise
of automatic updates is in the product's vision and most people will never open that
page. Written by hand, anything but `automatic` means off: guessing wrong that way
costs a release, and guessing wrong the other way reaches the network against
someone's wish. `nitid update` and the icon's menu ask once when someone asks,
whatever the switch says.

## Consequences

`tests/live_update.rs` runs the real binaries against a server of its own, under a
test's instance tag that now keeps `nitid install` out of the real registry, the
`PATH` and the desktop. It covers the whole path from the command and from the
resident, the registry byte for byte across an update with a planted default and
another program's entries, a wrong digest, a mislabelled release, an older one, the
switch, two updates with the oldest copy running, and a copy that is not installed.

Those tests found what reading had not. Starting the resident kept the output of
whatever ran `nitid update` open for ever, because a process with redirected output
holds that pipe under a second handle it has no name for, so children are now started
with nothing inherited, and in the executable's own directory rather than pinning the
caller's. And retiring a file over a name already taken deleted the copy an undo was
about to restore.

The release workflow installs each release, on a clean machine, over the one before
it with the one before's own updater, through the real address. That is the only check
that can see a break between versions, and it can see it before anyone is stranded.

The first release with an updater cannot be reached this way; it arrives by the
script. Its own update path was rehearsed against its real release on the development
machine, with `NITID_UPDATE_CURRENT` standing in for an older version.

A metered connection is not treated differently. The daily question is a few hundred
bytes, but the download is not.
