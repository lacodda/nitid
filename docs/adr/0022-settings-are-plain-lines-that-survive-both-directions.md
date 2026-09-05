# 22. Settings are plain lines that survive both directions

Date: 2026-09-05

## Status

Accepted.

## Context

Until v0.24.0 the settings file held three keys, all of them about where the
window was, and nothing read it but the window. v0.24.0 turns it into the
viewer's own settings: what the wheel does, when the chrome is on screen, how
a picture is framed when it arrives, where the clipping zebra draws its lines.
Fifteen values now, and the plan puts another handful in every version after
this one.

That is the point at which the format stops being an implementation detail. A
serialiser — `serde` with TOML or JSON — is the usual answer and would cost one
dependency and one derive. It also brings a failure mode worth naming: most
such readers treat an unknown key as an error, or drop it on the next write.

The second half matters more than it looks. A person who runs a newer nitid,
gets its settings, and then opens an older build — from a second machine, a
portable copy, a rollback after a bad release — has one file being written by
two versions. If the older build drops what it does not recognise, one run of
it silently empties everything the newer one stored.

## Decision

The file stays a list of `key = value` lines, parsed by hand, and a key the
running version does not know is kept and written back out untouched.

Neither half is about avoiding a dependency for its own sake. Line-by-line
parsing is what makes the file readable and repairable by the person whose
settings they are — the same reason the window placement was written this way
in v0.11.0. Carrying unknown keys through is what makes two versions able to
share one file without either destroying the other's work.

Everything else follows the same rule the placement already followed: every
failure is silent and falls back to the default. A word the setting does not
recognise, a number outside the range that could mean anything, a line with no
separator — each takes the default rather than refusing to start. A viewer
that will not open because its settings file has a typo in it has its
priorities backwards.

Values are clamped rather than trusted. A zoom step of 40 per notch, or a
zebra threshold outside 0..1, parses fine and would produce a viewer nobody
can use; the range is enforced where the file is read, not where the value is
spent.

## Consequences

Adding a setting is three lines — a field, a parse arm, a render line — and a
round-trip test that the defaults survive the file. There is no schema and no
migration step; a version that stops using a key simply stops knowing it, and
the key rides along in the unknown set until something claims it again.

The cost is that the format has no types beyond what each arm parses, so
nothing catches a value written in the wrong shape except the arm that reads
it. That is why each arm validates rather than assumes, and why the tests
cover the nonsense cases as well as the good ones.

The file is written on every change rather than batched: it is small, the
write is not on any path that matters, and a viewer that batched them would
lose settings on a crash.
