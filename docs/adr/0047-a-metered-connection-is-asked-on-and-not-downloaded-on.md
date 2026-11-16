# 47. A metered connection is asked on and not downloaded on

Date: 2026-11-16

## Status

Accepted. Settles the question ADR 0043 left open in its last paragraph.

## Context

The updater makes two requests: a few hundred bytes for the newest release's
manifest, once a day, and the release itself, about eighteen megabytes, when
the manifest names something newer (ADR 0043). It treated every connection
alike, so a laptop tethered to a phone spent a slice of somebody's data plan on
a viewer nobody had asked to update.

Windows knows what a connection costs. A person marks a network as metered, or
the carrier's plan says so, and `Windows.Networking.Connectivity` reports it:
`NetworkCostType` is `Unrestricted`, `Fixed`, `Variable` or `Unknown`, and the
same object says whether the machine is roaming or past its data limit.

## Decision

**The question is asked on any connection; the download waits for one that does
not count the bytes.** The manifest is too small to matter, and asking keeps
*Settings → Updates* truthful about what is out. When the answer is a newer
release and the connection is metered, the release is noted in the updater's
state as held and nothing more is fetched.

**What is held goes as soon as the connection allows.** The resident looks at
the clock once an hour; when something is held it also looks at the connection,
and fetches on the first look that finds it open. Waiting for the next daily
question instead would strand the laptop that is on a hotspot at the same hour
every morning. The mark is taken off before the download starts rather than
after it, so a release that fails to install is tried again the next day, not
every hour.

**A person asking is not made to wait.** *Check for updates* on the icon's menu
and `nitid update` fetch on any connection: whoever pressed it has decided what
their connection is for, and "there is an update, but not for you, not here"
answers a question nobody asked.

**Metered means a counted plan, roaming, or a spent limit.** `Fixed` and
`Variable` are the plans that count. Roaming and being over the limit count
whatever the plan is called, since those are the two ways an unrestricted plan
still bills. `Unknown` is open: most connections say nothing about themselves,
and a copy that waited on all of them would never update.

**When Windows cannot say, the connection is taken as open.** No connection
profile, or the call failing, costs at worst one download on a connection that
did not say what it was. The other way round is a copy that silently stops
updating, which is the failure ADR 0043 exists to prevent.

**No setting.** The switch in *Settings → Updates* stays the only one. Someone
who wants updates on a metered connection presses *Check for updates*; someone
who wants none at all has the switch.

**The decision is a function that knows nothing of Windows.** `Cost::network`
turns what Windows said into open or metered, `fetches` says whether a known
release is fetched now given who asked, and `held_can_go` whether a held one
may go. The WinRT call only fills in the `Cost`. One more feature of the
`windows` crate, `Networking_Connectivity`, and no new dependency.

## Consequences

The state file gains a `held` line. Copies before this one pass over a line
they do not know and drop it when they next write the file, which costs
nothing: they never held anything back.

*Settings → Updates* says when a release is held and what fetches it now.

`NITID_UPDATE_NETWORK` lets a test say what the connection is. It is heard
only beside `NITID_UPDATE_FEED`, which itself accepts only this machine, so
nothing a person runs can be told what its network costs.
`tests/live_update.rs` holds the whole path with the real binaries: a metered
resident asks and fetches nothing, the next one on an open connection installs
without waiting for the day to come round, and `nitid update` on a metered
connection installs.

The real call is covered only as far as a machine can cover it without a
hotspot: a test asks Windows from a thread that has set up no COM, as the
updater's thread has not, and holds that the answer is not a refusal for that
reason. That a network marked metered in Windows' own settings is reported as
`Fixed` is Windows' documented behaviour and was not rehearsed on a phone.
