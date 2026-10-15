---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-10-14
---

# Agents in the chrome

**TL;DR.** Agent sessions reach the chrome only as attention on the tab that
runs them. There is no agent list, row, inspector or state word: a tab whose
agent is waiting on an answer shows the same mark a bell does, and nothing else
about agents is drawn by default.

## Product behavior

- Agent resources remain provider-owned and project under their parent
  terminal (`workspace_projection.agentRowsUnder`). Resource identity is
  independent of window, tab, pane, and catalog position; multiple agents may
  share a terminal, and split terminals remain distinct parents.
- Only a current blocked report needs a person. It raises its parent
  terminal's attention through the same predicate as a bell
  (`terminalNeedsAttention`), and the tab aggregates attention from every
  split. Closing the resource lowers it on the next published update.
- Default top tabs in every window and the side rail show the mark beside the
  terminal control, with the accessible name `Needs attention: <tab title>`.
  The control's captured tab target is unchanged; the mark grants no command
  authority.
- Working, done, unknown and gone states are never shown, and neither are
  resource identities, parents, native ids or catalog evidence.

## Wire

The snapshot carries agent state only as each tab's attention flag. Extension
kinds 1 and 6 (the former tab agent rows and the identity-bound roster) are
retired and never reused; a decoder steps over them like any unknown kind.
`cockpit.navigation` request kind 5 is retired with them.

## Acceptance evidence

`src/tests/agent_session_rows_tests.zig` exercises real provider fixtures: rows
project under their parent, a blocked agent raises and a close lowers the
terminal's attention, an agent session is never a terminal surface, and the
snapshot's tab attention covers a blocked agent under a nonfocused split.
