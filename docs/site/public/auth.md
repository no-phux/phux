# phux auth.md

How agents and automated tools authenticate against phux resources. phux.sh
is a static site plus read-only hosted services: there is nothing to log in
to here, and no agent registration flow.

## This site: phux.sh and docs.phux.sh

All content is public. No API keys, no bearer tokens, no rate-limited
endpoints. Pages are served as HTML by default and as markdown when the
request sends `Accept: text/markdown`.

## Hosted MCP server: https://phux.sh/mcp

The MCP endpoint (`tools/list`, `tools/call`) is read-only and anonymous. It
exposes only public facts: docs search, page markdown, install commands, and
release metadata. It never accepts credentials and never returns user data.

## Hosted demo: wss://shell.phux.sh/session

Anonymous edge sessions require no authentication. Native Linux shell
sessions use an OAuth 2.0 browser flow delegated to GitHub or Google
(`https://phux.sh/auth/github`, `https://phux.sh/auth/google`); the resulting
session is an HttpOnly `__Secure-` cookie scoped to `phux.sh`, never a
bearer token, and it is only valid for the demo. The demo does not issue
credentials usable elsewhere.

## phux the server (self-hosted)

phux servers are first-party software you run on your own machines. Consumers
attach over the phux wire protocol; operator-controlled enrollment and paired
workload authentication are configured on the server, not with this site.
See the [remote access](https://docs.phux.sh/remote-access) and
[wire protocol](https://docs.phux.sh/wire/proto) docs for how access is
granted and revoked on a server you control.

## Registration

### phux.sh hosted services (this site + /mcp + the demo)

No registration exists or is required. The hosted MCP server is read-only and
anonymous, and the demo's optional OAuth login is a browser flow for humans,
not an agent credential. Do not send credentials to phux.sh; it will never
ask for them.

### A phux server you operate (agent registration)

phux servers are self-hosted, so **registration is operator-provisioned
pairing** — there is no signup endpoint, no accounts, and no phux-hosted
identity. The supported method:

1. **Provisioning.** Someone who can already administer the host (via ssh)
   mints a remote credential:
   `phux pair --json` (ADR-0031). This writes a token to the server's token
   store (`PHUX_WS_TOKENS`) and prints the credential ID, the wire endpoint
   (`wss://<host>:<port>`), and the certificate fingerprint.
2. **Credential use.** The agent presents the bearer token and verifies the
   fingerprint when it attaches over the wire protocol. The credential is
   a capability: anyone holding it can attach, so treat it like an ssh key.
3. **Rotation and revocation.** `phux pair rotate <credential-id>` replaces
   the bearer secret with a bounded overlap; `phux pair revoke <credential-id>`
   invalidates it immediately. Both are operator commands on the server, not
   API calls.

Details and the full remote-access flow:
[remote access](https://docs.phux.sh/remote-access),
[wire protocol](https://docs.phux.sh/wire/proto). To use phux
programmatically from a tool, install the CLI
(`curl -fsSL https://phux.sh/install | sh`) and attach over the wire, or
point your MCP client at a `phux mcp` process running next to your own
phux server.
