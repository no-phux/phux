---
group: connect
order: 4
---

# Conversations

Talk to a friend or an agent using native phux mail. No Multiplayer account,
friend request, party, or additional daemon is needed.

## Invite, join, talk

The inviting machine must already run a **reachable, paired phux hub with a
QUIC listener**. Both people need compatible phux releases. The hub may be
one person's computer, a machine on a shared tailnet, or a server they own.

On the hub:

```sh
phux chat invite buddy --hub hub.example:8788 -m "Hey, we're connected."
```

Share the printed `phux://chat/v1#...` invitation privately. It is a secret,
works once, and expires unused after ten minutes. It pins the hub's CA,
the participants' addresses, and the actual first message.

On the friend's computer:

```sh
phux chat join patrick
```

Paste the invite and press Enter. The local names `buddy` and `patrick`
may differ. Joining displays the actual welcome received from the hub and
stores a correlated confirmation. It never prints "ready" from a TLS
handshake alone.

On either side, use its local conversation name:

```sh
phux chat open buddy
phux chat send buddy -m "Here's the setup trace."
phux chat read buddy
phux chat status buddy
```

On the friend's machine, substitute `patrick`. In the line-oriented view,
type a message and Enter; `/quit` leaves the view, not the conversation.

`invite` runs on the hub that owns the shared mail store. When that hub is
a VPS, run the inviting side there, for example in your own remote phux
session. The friend's computer needs no inbound listener or local terminal
server.

## What ten seconds means

The target is **invite to a received welcome and confirmed response**, with
phux installed and a compatible hub already reachable. Installation,
hosting a server, firewall changes, Tailscale sign-in, and exchanging the
invitation by hand are not hidden inside that timing.

The current messaging route needs UDP QUIC to the hub. An ordinary HTTP
tunnel is not enough. phux's blind terminal relay does not yet carry this
coordination route; passing a relay address as `--hub` does not enable it.

For a dedicated hub, configure:

```toml
[policy]
mode = "paired"
```

Initialize its authority and start an explicitly bound listener:

```sh
phux workload authority --init
phux server --quic 0.0.0.0:8788
```

Allow UDP 8788 at the server and advertise the address the friend can
actually reach. Do not replace a running workstation's config or assume
config reload creates a new listener. See [remote machines](./remote.md)
for pairing, services and network setup.

## Bring an agent

Run one supported agent with its installed phux integration:

```sh
phux chat run buddy -- pi
phux chat run buddy -- opencode
phux chat run buddy -- claude
```

This sets `PHUX_CHAT_RECORD` to the exact private record and both phux
executable variables to the launching binary. Only that process tree's
**mail** uses the conversation. The CLI validates the record and defaults
mail to its inbox; an explicit `--as` still wins. `PHUX_AGENT_ADDRESS` is
not changed: claims retain both their normal store and holder identity.
Nothing installs an integration or turns an arbitrary program into a chat
listener. A missing or malformed selected record fails closed.

pi, Oh My Pi and OpenCode can deliver to an idle agent conversation.
OpenCode must show the receiving session in its TUI. Claude Code and Grok
receive at their next hook; an already idle prompt is not automatically
woken. Run only one receiving agent per conversation inbox.

Incoming mail stays a colleague's request, not an instruction from your
user. Generated reply commands include the exact phux executable and
`--record PATH`, so a shared harness server with another HOME or profile
cannot accidentally choose another mailbox. MCP mail tools also accept an
explicit `conversation_record`; they do not infer one from a shared
server's environment.

## Addresses, history and receipts

Each participant gets a randomly named enrolled namespace and a local
`inbox` address. The private record pins both addresses, the hub, and its
certificate trust. `chat status --json` exposes those addresses when a
tool needs them; ordinary conversations do not require finding pane IDs.

Mail is durable in the hub's existing work store, normally
`~/.local/state/phux/work/work-v1.sqlite`. Conversation records and
owner-only enrolled identity files live in this profile's
`chats/NAME/` directory. They do not add a global `[[uplink]]` entry or
redirect unrelated mail and claims.

The welcome and its confirmation use separate handshake threads, so
ordinary conversation traffic does not evict the readiness evidence from
the newest-500-message history view. `read` and `open` do not mark mail read
or take it for harness delivery. Viewing a message does not steal it from
the receiving agent.

- **Welcome stored:** the hub has the real first message.
- **Welcome received:** the joining CLI fetched that exact message from
  the expected participant.
- **Peer confirmed welcome:** the other participant returned a correlated
  receipt naming that welcome and conversation.
- **Stored on hub:** a later send committed. This is not proof that a
  human read it or an agent handled it.

Agent delivery remains unverified until the receiving harness actually
shows the message or returns a reply.

## Trust and failure recovery

The invitation enrolls a native mail-and-claims credential, never ordinary
terminal pairing. Neither participant exposes its owner socket or terminal
inventory. This is **not** a per-conversation sandbox: the existing
coordination role can send to other hub addresses and list claims. Keep a
collaboration hub separate from sensitive coordination if those privileges
are inappropriate.

Traffic is encrypted and the hub's CA is pinned. **The hub sees and stores
plaintext messages.** This is not end-to-end encryption against the hub
operator. The credential authenticates its namespace; names inside it do
not independently prove which person or bot wrote a message.

A chat invite has a distinct versioned format that ordinary `host join`
rejects. No one has to remember a second `--mail-only` flag.

If joining enrolled successfully but a later mailbox exchange failed,
retry `phux chat join` with the same local name: it resumes the saved
identity without consuming another invite. An expired or reused invite
needs a fresh code. Neither a network error nor an invalid record falls
back to a local store.

If creation failed after saving its identity, rerun `chat invite` with the
same name, hub, and identical welcome. The welcome send replays safely.
Once an invitation was issued, that alias cannot issue another; for an
expired or lost invitation, withdraw the old ticket and use a new alias.
An identity-storage failure is different: preserve the directory named
in the error rather than deleting issued keys.

After an uncertain send, retry with the **same operation id and identical
text** using `--operation-id`; do not invent another id. The CLI reports
that id even when the reply was lost.

To withdraw an unused invitation, use the ticket id printed by `invite`
with `phux workload revoke` on the hub. For an enrolled participant, revoke
its credential id from the conversation record on that hub. Removing a
local alias or forgetting a host is not credential revocation.
