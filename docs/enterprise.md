# Organizations

What phux does for a company that runs coding agents on many machines, and
where the line sits between the open-source core and the paid service.

## The boundary

Everything a server needs to be governed ships here, under Apache-2.0, and
works with no account:

- [Managed credentials](./remote.md#managed-by-an-organization): a server
  accepts client certificates issued by an organization's certificate
  authority, for the keys and scopes the organization syncs into its
  registry.
- [The audit log](./audit.md): a signed, hash-chained record of every
  connection, command, refusal, approval and credential change, with no
  terminal bytes in it.
- [Held actions](./agents.md#safety) on any approvable verb, and delegated
  keys that record who started them.
- The [`lts` release channel](./releasing.md).

The paid service (phux for teams) runs the parts an organization needs and
one person doesn't: sign-in with the company's identity provider, group
policy turned into per-host grants, a central audit store with search,
retention and export, a blind relay, and support. Its code is not in this
repository.

## What stays true

- Billing never locks anyone out of their own server. A lapsed organization
  loses central policy changes; terminals, pairing and local grants keep
  working.
- No phux-operated service sees terminal bytes. The relay forwards
  ciphertext; the audit store receives metadata.
- The registry is the only admission authority. An organization's
  certificate authority admits nobody by itself: a key gets in only while a
  registry record names it, and removing that record ends its live
  connections.
- A control-plane outage does not stop work. Servers keep their last
  registry and buffer audit records locally until they can be shipped.

## Decisions

- Managed credentials skip the pairing bearer. The bearer is a second
  secret for certificates a server issued itself; for an organization's
  certificate, the short-lived certificate plus the registry record the
  organization synced are the authority, and the bearer would only be a
  shared placeholder.
- Audit is a local file first. Each server writes its own chain, so a
  record exists even when nothing ships it, and a central store can detect a
  rewritten history by the broken chain.
- Organization features are a separate, private product built on these
  public mechanisms. A feature that makes a server safer for everyone lands
  here, not behind the paywall.
