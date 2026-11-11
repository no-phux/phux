# Mail across a hub

How `phux mail` and `phux claim` work on a machine joined to a hub. Part of
[mail and path claims](./agents.md#mail-and-path-claims).

On a machine joined to a hub with `phux host join`, such as a
[Minimal box](./remote.md#minimal-boxes), `phux mail`, `phux claim` and the
MCP mail and claim tools use the hub's store instead of their own. Agents in
boxes and on the hub then read the same mail and see the same claims, and the
hub's agents' harnesses deliver mail from boxes like any other.

Each side writes addresses as it would at home. For a box joined as
`min-api`:

| On the hub | In the box |
| --- | --- |
| `@749` | `hub/@749` |
| `min-api/@3` | `@3` |
| `min-web/@2`, another box | `min-web/@2` |

```sh
phux mail send hub/@749 -t fix-login -m 'tests pass in the box'   # in the box
phux mail send min-api/@3 -t fix-login -m 'merge it'               # on the hub
```

**A box speaks only as itself.** The hub puts every sender, reader and
claim holder a satellite names under that satellite's name, the one its
credential was issued for. A box can't read, take or acknowledge the hub's
mail, or renew or release the hub's claims. It can write to any address and
list every claim. Mail and claims are all it reaches: it still opens no
terminal on the hub.

The satellite dials the hub's QUIC listener with the credential it joined
with, so this needs nothing beyond the join. When the hub can't be reached,
the command fails; it never falls back to the box's own store. Revoking the
satellite's credential on the hub cuts it off. A hub that adds a machine
with `phux host add --role satellite` dials that machine instead, and that
machine keeps its own mail.

Claims compare paths as each side writes them. A box usually sees the
project at a different path from the hub, so its claims show up on the hub
but don't conflict with the hub's own.
