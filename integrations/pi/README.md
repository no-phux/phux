# @phux/pi

Pi package for selecting, orchestrating, and supervising shared phux terminals
through the external canonical CLI. It appends cache-preserving fleet context
at user-turn boundaries and provides bounded pane inventory, literal paste,
acknowledged agent turns, finite screen/agent/resource waits, runtime discovery,
spawn/launch with local placement, insert/move/swap topology edits, confirmed
kill/destructive-signal controls, tags, asks, finite event collection, composited
snapshots, and branch-local aliases/groups. Input and destructive actions reject
Pi's hosting pane after target resolution; broad raw write selectors are refused.
It does not expose pairing credentials or emulate missing CLI operations.
Input-authority tools are blocked upstream: the CLI's connection-scoped lease is
released as soon as a one-shot invocation disconnects, so it cannot provide a
durable Pi tool action. Installation, compatibility,
the exact tool and command surface, lifecycle behavior, and safety boundaries
live in the canonical [Pi integration guide](../../docs/consumers/pi.md).

## Development and validation

Install the locked development dependencies from this directory:

```sh
npm ci
npm run gates
```

The real integration harness is opt-in:

```sh
PHUX_PI_REAL_SMOKE=1 npm run smoke:real
```

`pack:check`, `smoke:load`, and `smoke:real` own the package-local artifact and
harness checks. Consumer setup and operational interpretation remain in the
[Pi integration guide](../../docs/consumers/pi.md).
