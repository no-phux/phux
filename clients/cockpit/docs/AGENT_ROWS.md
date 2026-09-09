---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-09
---

# Parent-bound agent rows

**TL;DR.** Agent resources remain provider-owned rows. The rail names the
actual resource and terminal parent; the Agents inspector pages the complete
attached roster and jumps through the existing fenced terminal navigator.

## Product behavior

- Multiple agents may share a terminal; split terminals remain distinct parents.
  Resource identity is independent of window, tab, pane, and catalog position.
- A rail row shows provider, reported state, and parent/resource identity.
  Only a current blocked report receives attention. Closing a resource removes
  it on the next published catalog/closed-record update.
- Agents is available from every window and from the keyboard-accessible
  workspace switcher. Its one-resource pages expose resource identity, parent,
  producer-native ID, and the catalog/record evidence actually held by the host.
  Previous/Next and arrow keys reach every row. Enter or Jump to parent uses
  the existing terminal navigator; an absent parent has no jump.
- An automatic refresh cannot retarget the inspected resource after a close or
  session switch. If its page now names a different identity, inspection is
  withdrawn with an explicit notice; Refresh starts a new catalog selection.
- The inspector describes last reported state, not liveness. Offline,
  reconnecting, and delayed engine snapshots qualify that report. There is no
  timestamp or transcript claim: the current provider retains neither here.
  A missing record-derived state explicitly says records have not been observed.
- Snapshot capacity and tab-strip capacity are presentation bounds, never
  inventory bounds. A visible Agents count opens the paged inspector, including
  resources whose parent is not currently placed. Local PTYs report no agents.

## Typed internal contract (defined before implementation)

The existing 4096-byte snapshot and length-delimited extension framing remain.
Kind 1 is the legacy tab-only row format. Kind 2 carries tab command contexts.
Kind 5 is the identity-bearing agent format:
`total:u16, count:u8`, followed by rows with `window:u8, tab:u8,
parentNavigationIndex:u16, state:u8, attention:u8, providerLength:u8,
resourceLength:u16, parentLength:u16`, then the three UTF-8 strings. Canonical
identity text is `phux:<route-kind>:<resource-number>@<host>` (host may be empty).
Identity strings are never truncated. Provider display slugs may be visibly
elided. Rows consume only remaining snapshot capacity; total counts the complete
host roster. The bounded row prefix is not a second agent registry.

`cockpit.navigation` request kind 5 selects the agent inspector. It uses the
same revision/offset/query header as kind 3; query is empty, page size is one.
(Kind 4 carries scoped navigation requests upstream.)
The response echoes the request, then `total:u16, count:u8`, and the usual
`parentNavigationIndex:u16, labelLength:u8, label`, followed by four `u16`
length-prefixed UTF-8 fields: resource, parent, native ID, evidence. Each field
is bounded by the existing provider identity/text bounds. No full transcript,
artifact, durable work, or execution authority is manufactured. Missing parent
navigation uses 65535 and disables Jump, while inspection remains available.

The parent index resolves the exact provider-qualified terminal in the existing
unfiltered navigation catalog at the same engine revision. The native revision
fence rejects moves, closes, switches, or stale clicks. Resource identity is
display/inspection data; it is never sent as terminal input or replica identity.

## Acceptance evidence

Node contract tests cover identities, stale/offline projection, removal, paging,
and fenced parent commands. Native tests exercise actual provider fixtures,
split-parent resolution, close, bounded overflow and inspector evidence. The
shipping markup gate and serial real producer create/blocked/close acceptance
are required integration checks; headless snapshots do not prove host rendering.
