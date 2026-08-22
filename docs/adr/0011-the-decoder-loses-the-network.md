# 11. The decoder loses the network, and the bridge sheds a false cost

Date: 2026-08-22

## Status

Accepted. Closes the gap recorded in [ADR 0002](0002-sandbox-c-decoders.md)
and [ADR 0009](0009-heavy-decodes-run-in-a-child.md), and corrects two
findings of ADR 0009 that turned out to be wrong.

## Context

The decoder child ran behind a restricted token at low integrity, which
confines everything except the network: a decoder taught to try reported
`listen=true connect=true` from inside that boundary (v0.9.0). Closing the
network needs an AppContainer. The attempt in v0.9.0 was reverted with the
finding that the container's SID must be granted filesystem access along the
whole directory chain to the executable — which would have meant the viewer
editing permissions on folders it does not own.

Separately, ADR 0009 measured a 12-megapixel HEIC at ~830 ms in-process
against 1060–1220 ms across the boundary and attributed the difference to
48 MB of pixels crossing a pipe; the owner's decision was to make the
crossing cheaper with shared memory.

## Decision

**The decoder runs in an AppContainer with no capabilities.** The profile
(`lacodda.nitid.decoder`) is registered lazily on first use and removed by
`nitid uninstall`. The launch is a direct `CreateProcessW` with a
`PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute, because
`std::process::Command` has no stable way to pass one. On a machine where the
profile cannot be registered, the launch falls back to the previous
restricted-token arrangement and says so on stderr once — never silently.

**The pixels come home through a shared memory section.** The viewer reserves
a section as large as the protocol allows (`SEC_RESERVE`, so untouched pages
cost nothing), duplicates the handle into the still-suspended child, and
names it in the request; the reply then carries metadata and a length rather
than the pixels themselves. The handle crosses by `DuplicateHandle`, not
inheritance — inheritance is process-wide, and two decodes spawning
concurrently would each leak their section into the other's child. The pipe
reply remains as the fallback and stays tested.

**The pipes travel by an explicit handle list.** With `CreateProcessW` in our
hands, `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` narrows inheritance to exactly the
child's two pipe ends, closing the same concurrent-spawn leak for handles in
general.

## What the measurements overturned

Both findings this stage inherited from v0.9.0 were wrong, and both fell to
direct measurement:

- **The container needs no filesystem grants at all.** `CreateProcessW` maps
  the executable image with the *viewer's* access; the child never opens its
  own path. A container child starts and decodes from a directory whose ACLs
  never heard of its SID — the network tests run it straight out of
  `target/`. The v0.9.0 "file not found" was in all likelihood that
  attempt's own fat-pointer bug, which its notes also recorded. `nitid
  install` therefore touches no ACLs.

- **The pipe was never the cost.** With the transport switchable, section
  and pipe decode a 12-megapixel HEIC in the same time to within noise;
  48 MB crosses a pipe in single-digit milliseconds. What the boundary
  actually cost per decode was ~35–45 ms of `CreateToolhelp32Snapshot` — a
  system-wide thread walk to find the handle `Command` had thrown away, now
  gone because `CreateProcessW` returns it — and ~90–110 ms of child process
  initialisation, which remains and is the honest floor of the boundary. The
  section is kept: it makes the reply cost independent of image size, which
  gigapixel images (v0.14.0) will want, and it is already tested.

## What "closed" means, measured

`tests/sandbox.rs` runs the probe decoder inside the real container:

- **Outward** — a live listener just outside the sandbox is unreachable:
  `connect=false`. This is the exfiltration direction, and the one that
  matters most.
- **Inward** — the decoder can still *bind* a loopback listener (binding is
  not gated), but nothing gets through: the test hammers the bound port from
  outside for the whole window and the child accepts nothing
  (`accepted=false`).
- **The probe can tell confinement apart** — the child reports
  `TokenIsAppContainer` about itself, and the same probe behind the
  restricted-token fallback sees an open network (`connect=true`). A broken
  probe, a firewall, or a quiet fallback would fail these assertions loudly.

## Consequences

The decoder now runs with no filesystem, no network in either direction, no
privileges, a capped job object, and dies with the viewer — and every one of
those words is held by a test rather than a comment. The per-decode overhead
is ~90–110 ms of child initialisation on the measuring machine; making that
cheaper (a warm decoder, reused across decodes) is a possible later stage,
recorded in the hub backlog rather than promised here.
