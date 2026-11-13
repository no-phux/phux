# Audit log

The phux server keeps a signed, append-only record of who connected and what
they were allowed to do. Each record is chained to the one before it, so a
deleted, reordered, or edited record shows up when the log is checked.

## Where it lives

`audit/` in the profile state directory
([File locations](./reference/files.md#state)):

| File | What it is |
| --- | --- |
| `audit.jsonl` | The live log: one JSON record per line, owner-only (`0600`). |
| `audit.jsonl.1` .. `.4` | Older records. The live file rotates at 64 MiB and the oldest file is dropped. |
| `audit-key` | The ed25519 signing key (PKCS#8 DER, `0600`). |
| `audit-key.pub` | The public key, as 64 hex characters. |

The directory is `0700`. The server creates the key the first time it starts
and keeps it from then on. Copy `audit-key.pub` somewhere else if you want to
check the log against a key the host cannot quietly replace.

## What is recorded

| `kind` | When |
| --- | --- |
| `connection.open`, `connection.close` | A client connects or disconnects, on any transport. |
| `connection.refused` | A remote peer fails admission: no enrolled workload certificate, or a missing or wrong pairing token. |
| `command` | A command is authorized, or a frame that changes state (spawn, move, attach, metadata write) is. |
| `command.refused` | A command or frame is denied by policy. |
| `approval.held`, `approval.decided` | A command waits for approval, then is approved, denied, expires, or is withdrawn. |
| `credential.added`, `credential.removed` | A workload credential appears in, leaves, or is revoked from the registry. |
| `file.get`, `file.put` | Each authorized `GET_FILE` or `PUT_FILE` chunk, with its offset and size. |
| `input.lease` | A Terminal's input lease is taken or given back. |
| `clipboard.read` | A client answers a pane's OSC 52 clipboard read, with the pane, selection, and byte count. |

An authorized keystroke, paste, input command, or resize is not recorded.
Taking input is recorded once, as `input.lease`. A denied one is recorded,
and an uncorrelated denial at most once a second per connection.

## What is never recorded

Terminal output, keystrokes, paste payloads, clipboard text, file contents,
pairing tokens, and upload ids never reach the log. A record names the command, its target
(`terminal:3`, a path, a credential id), and small facts such as an offset or
a byte count.

## Record format

Each line is one `phux.audit/v1` record:

```json
{"v":1,"seq":41,"ts":"2026-11-09T18:02:11.204Z","host":"mini",
 "kind":"command.refused",
 "actor":{"credential":"sha256:9c1f...","principal":"automation","subject":null,
          "on_behalf_of":null,"transport":"quic"},
 "action":"KILL_RESOURCE","target":"terminal:3","outcome":"denied",
 "detail":{"client":12},
 "prev":"5e0b...","hash":"a41c...","sig":"MEUCIQ..."}
```

- `actor.transport` is `local`, `quic` (WebTransport included), `wss`, or
  `ssh`. A local connection has no credential. An action the server took
  itself, such as an expiry or a registry change, has an actor with every
  field `null` and `transport` `local`.
- `outcome` is `allowed`, `denied`, `held`, `approved`, or `rejected`. An
  expired or withdrawn approval is `rejected`, and `detail.ended` says which.
- `detail.client` is the connection id, so a connection's records can be
  read together.
- `hash` is the lowercase hex SHA-256 of the record's canonical form: the
  record without `hash` and `sig`, as compact JSON with object keys sorted at
  every level (`actor` and `detail` included). Strings are escaped as
  serde_json writes them: `"`, `\` and control characters only, with UTF-8
  left as is. The stored line keeps the field order shown above.
- `prev` is the previous record's `hash`. The first record's `prev` is 64
  zeros, and the chain carries on across rotated files.
- `sig` is the standard (padded) base64 ed25519 signature over the 32 raw
  bytes of `hash`.

## Read and check it

Both commands read the files directly, so they work with no server running.

```sh
phux audit tail              # the last 20 records
phux audit tail -n 100 -f    # the last 100, then follow
phux audit tail --since-seq 4000 --json
phux audit verify            # exits 1 at the first bad record
```

`phux audit verify` checks that every line parses, that `seq` has no gaps,
that each `prev` matches, that each `hash` matches its record, and that each
signature verifies against `audit-key.pub`. It stops at the first failure and
names its `seq`, file, and line. When rotation has dropped the oldest file,
the check starts from the oldest record still on disk.

## When a write fails

A failed write never blocks or fails a command. The server logs a warning and
counts the failure. `phux doctor` warns while the count is above zero, and
`phux perf` shows it as `audit.write_failures`. The usual causes are a full
disk or a changed directory mode.
