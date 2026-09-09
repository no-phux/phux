# @phux/opencode

OpenCode plugin for operating shared terminals through an external local
phux server and appending cache-preserving fleet context. Installation,
configuration, tool behavior, target precedence,
lifecycle gaps, and safety boundaries live in the canonical
[OpenCode integration guide](../../docs/consumers/opencode.md).

## Package development

Install locked dependencies and run the deterministic, no-LLM gates from this
directory:

```sh
npm ci
npm run gates
npm run smoke:opencode
```

`smoke:opencode` starts an isolated OpenCode server and verifies that its
resolved config accepts the built plugin URL. Plugin setup, registration,
cleanup, and tool behavior are covered by `npm run gates`. The smoke requires
an `opencode2` executable; set `OPENCODE_BIN` to choose it.

The real packed-plugin smoke is opt-in and requires `phux >= 0.1.0`:

```sh
PHUX_OPENCODE_REAL_SMOKE=1 npm run smoke:real
```

It packs and installs the artifact in a temporary consumer, starts a private
phux server on a temporary socket, invokes public create/run/snapshot tool
definitions without an LLM, prints safe human attach argv, and tears down the
server on normal exit, `SIGINT`, or `SIGTERM`. It never uses the default phux
socket.
