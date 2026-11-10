# Held actions

A device or automation key enrolled with a held verb in its scope has those
actions held by the server until a person approves or denies them. Nothing
on this machine's own socket is ever held. Keys and scopes are in
[Remote machines](./remote.md#who-can-do-what).

```sh
phux approvals          # what is waiting
phux approve ID
phux deny ID
```

## What can be held

| Scope | Holds |
| --- | --- |
| `?signal@SEL` | Kills, signals, and forced detaches |
| `?terminate@SEL` | Ending or removing a session or pane (`SESSION_KILL`, `SESSION_DELETE`, `RESOURCE_TERMINATE`, `RESOURCE_REMOVE`) |
| `?input@page`, `?input@window`, `?input@display` (each optionally `:HOST`) | Input to documents of that class (`SEND_DOCUMENT_INPUT`, [Authority](./authority.md#control)) |

Other verbs cannot be held, and `?input` only on a control class: a frame
(not a command) that needs a held verb is refused, never held, so a held
`input` anywhere else would refuse keystrokes. File transfer (`GET_FILE`,
`PUT_FILE`) rides `input` on a pane or the server, so it is not holdable
either. An un-held grant that also covers the subject wins. A phux older
than a marker refuses the scope, at enrollment or at connect, rather than
grant the verb un-held.

## Deciding

Approving runs the held action once, as the key that asked, under that
key's own grant as it stands then. Deciding needs the held verbs un-held on
the same subject; the owner socket always may. An action nobody decides
within `defaults.approval-ttl-secs` is refused as expired and never runs.
