---
group: connect
order: 3
---

# Security

Control who reaches a phux server and what they may do: scoped keys,
read-only keys, held actions, the audit log, 1Password credentials, and
organization-managed credentials.

Two rules hold everywhere. **The local socket gives full access to any
program running as the server's user**; phux cannot tell those programs
apart, and nothing here is a sandbox against them. **A remote connection
needs an enrolled client certificate and a token** from pairing
([who can connect](./remote.md#who-can-connect)); organization-managed
credentials skip the token.

## Scopes

Every enrolled credential carries a scope ceiling, one or more
`<verb>[,<verb>...]@<selector>` grants. The verbs and selectors are listed
under [`phux workload add-key`](./reference/cli.md#phux-workload-add-key).
`phux host add` and `phux pair` enroll a person's device with full control.
Anything narrower is minted by hand on the serving host with
`phux workload add-key`, from a certificate signing request the client
generated; input containing a private key is refused.

```sh
phux workload authority --init     # create the CA once; prints the pinned fingerprint
phux workload list                 # id, scope, principal, active
phux workload grant sha256:ID observe@display
phux workload ungrant sha256:ID observe@display
phux workload revoke sha256:ID
```

A grant applies at the device's next connection. Ungranting or revoking
ends the live connections that relied on it, with no restart.
`--principal` (`human` or `automation`) decides input-lease precedence,
never scope.

## Read-only keys

A CI runner that lists sessions but reads nothing, and a collaborator who
lists sessions and reads only the ones you share:

```sh
phux workload add-key --scope inventory@host \
  --expires-in 604800 --principal automation \
  --json --cert-stdout < ci-runner-1.csr

phux workload add-key --scope inventory@host \
  --scope observe@session:KEY \
  --expires-in 2592000 --principal human \
  --json --cert-stdout < collaborator.csr
```

`inventory` is what lists (`phux ls`); `observe` on a `session:KEY`
selector reads that session's terminals and nothing else. `KEY` is the
session's full 26-character `key` from `phux ls --json`; add one `--scope`
per shared session. Never grant `observe@host` for this: it reads every
terminal on the host, secrets included. Do not add `input` or any other
verb, and do not enroll these holders with `phux host add`, which asks for
the full-control ceiling.

The JSON output's `credential_id` is the id to record (the registry stores
no label) and `certificate_chain` is the PEM for the client's
`PHUX_WORKLOAD_CERT`; its key stays in `PHUX_WORKLOAD_KEY` on the client.
Pass `--expires-in` explicitly: the default is 90 days. One certificate per
runner. The client still needs a pairing token (`phux pair --expires 30d`)
and registers the host with `phux host add NAME quic://HOST:8788
--token-file PATH --cert-fingerprint FP`.

**Rotate** with a new CSR and `--replace sha256:PREVIOUS`, passing the same
scopes and `--expires-in` again. Require `replaced: true` in the output: an
unknown id is accepted with `replaced: false` and revokes nothing. The
predecessor and its live connections end at once, with no overlap.

## Pages, windows, and displays

Typing into a mirrored page, window, or display, and watching a window or
display, is never inherited from a terminal grant, not even `*@global`. It
needs a grant naming the class: `input@page`, `input@window`,
`input@display`, `observe@window`, `observe@display`, each optionally
`:HOST` for a satellite, or `@resource:KEY` for one document. A mirror with
no class takes input only from the local socket. `phux host add` and pairing
give a person's device `observe@display` and `observe@window`; automation
keys get them only explicitly. A refused attach names the missing grant and
the command that adds it. Through a hub, both the phone's grant on the hub
and the hub's credential on the satellite need the class.
[Authority](./authority.md) has the rules.

## Held actions

A key whose scope marks a verb with `?` has those actions held by the server
until someone approves or denies them:

| Scope | Holds |
| --- | --- |
| `?signal@SEL` | Kills, signals, and forced detaches |
| `?terminate@SEL` | Ending or removing a session or pane |
| `?input@page`, `?input@window`, `?input@display` (optionally `:HOST`) | Input to mirrors of that class |

```sh
phux approvals
phux approve ID
phux deny ID
```

Other verbs cannot be held, and `?input` only on a control class. An
un-held grant that also covers the subject wins. Approving runs the action
once, as the key that asked, under its grant as it stands then. Deciding
needs the same verbs un-held on the same subject; the owner socket always
may. Undecided actions expire after `defaults.approval-ttl-secs` and never
run. Holds do not apply to the local socket. A phone with
[background push](./protocol-files.md#background-push) can decide
from the notification.

## Audit log

The server keeps a signed, hash-chained, append-only record of connections,
authorization decisions, approvals, credential changes, file transfer
chunks, forwarded connections and ssh-agent signatures, input leases,
clipboard reads, and links panes asked to open (the host only). It lives in `audit/` in the
[state directory](./reference/files.md#state), rotating at 64 MiB with four
old files kept.

**Never recorded:** terminal output, keystrokes, paste payloads, clipboard
text, file contents, pairing tokens. Authorized keystrokes and resizes are
not recorded; taking input is, once, as `input.lease`.

```sh
phux audit tail -n 100 -f
phux audit tail --since-seq 4000 --json
phux audit verify          # exits 1 at the first bad record
```

Both read the files directly, with no server running. `verify` checks
sequence gaps, each `prev` link, each `hash`, and each signature against
`audit-key.pub`; copy that file elsewhere to check against a key the host
can't quietly replace.

Each line is a `phux.audit/v1` record. `hash` is lowercase hex SHA-256 of
the record without `hash` and `sig`, as compact JSON with keys sorted at
every level and strings escaped as serde_json does. `prev` is the previous
record's hash (64 zeros first). `sig` is padded base64 ed25519 over the 32
raw hash bytes. A failed write never fails a command; `phux doctor` warns
and `phux perf` counts `audit.write_failures`. A second server on the same
state directory runs without a log rather than fork the chain.

## Program resume

Off unless `[restore] program-resume` is true. A program that knows the
pane's `OSC7577_TOKEN` can report a command. phux checks a SHA-256 of the
report and that token, a 60-second window, and that the reporting process is
a same-uid direct child of the pane's shell. Text from `cat`, `tail -f`,
`ssh`, or `less` does not know the token, so a planted report is ignored.

The token is only in the first process's environment and, across
`phux upgrade`, in the upgrade handoff. It is not metadata, `--json`, or a
log line. The command itself is stored with the pane and shown, escaped, by
`phux ls -l` and `phux restore --dry-run`. Treat that command like a shell
startup file: it can name files you had open.

Nothing runs unless `[[restore.trust]]` lists the program. There are no
built-in entries. phux never resumes one when it is running as root. A record
that is untrusted, or whose directory is gone, starts a shell. A resume that
was already tried is not tried again. The legacy OSC 7501 `resume=` token is
still unauthenticated; new programs should use OSC 7577.

## Kept scrollback

Off by default. With [`[restore] scrollback`](./concepts.md#kept-scrollback)
the server writes what each pane last printed to disk, and that can hold
secrets. `org/v1.scrollback/` in the [state directory](./reference/files.md#state)
(0700, one 0600 file per pane; a server on another socket has its own beside
its store in `org/sockets/`) holds the last lines of each pane's main screen,
as text and colors.

- The files are not encrypted; use full-disk encryption. Your own backups of
  the state directory include them; phux's reports, archives and logs do
  not.
- A pane's file is deleted within a second of the pane being killed, closed
  or removed, and at the next server start after a crash. A process that
  exits in a pane that stays keeps its file.
- `scrollback-exclude` keeps a session, a pane (by its `phux rename` name) or
  a program out. Moving or renaming a pane into it deletes its file.
- `scrollback = 0` deletes every file when the server next starts.
- Clearing a pane's history (`CSI 3 J`) clears its file a few seconds later.
- The text is stripped of everything but colors when written and again when
  read, and goes only into the pane's history: never to a program, agent
  detection, or `phux wait` on the screen.

## 1Password credentials

Supply 1Password secrets to a program only when you approve each request.
phux stores references, never a vault password, token, or copy of a value.
On the device with the 1Password desktop app and CLI (enable **Settings →
Developer → Integrate with 1Password CLI**):

```sh
phux credentials serve --name laptop --account my.1password.com
phux credentials serve --name laptop --remote build-box   # broker on another server
```

Where the program should run:

```sh
phux credentials run --provider laptop \
  --env OPENAI_API_KEY=op://Development/OpenAI/credential \
  --purpose 'Coding agent for the API project' -- codex
```

`run` always executes locally; `--remote` picks the broker. The request
waits for approval with `phux approve ID` on a terminal, after you check the
exact references, provider, and requester. The purpose is the requester's
claim, not verified identity. Approval covers one request. The provider then
runs `op read`, 1Password handles its own unlock, and the values enter the
program's environment, never its arguments.

- Every request needs approval, even on the owner socket. `approve --yes`
  and MCP cannot approve credentials; they can deny. A client approves only
  if it declares it shows the exact request (HELLO field 13,
  `CREDENTIAL_APPROVALS`).
- Requesting, providing, and deciding need `bind` and `input` on global, so
  a delegated agent key can't.
- The broker server sees the values in memory. Don't broker through a server
  you don't trust.
- The program can leak its environment, and keeps the values until it exits.
- A request expires within 120 s or the approval TTL, unlock included; any
  failure releases nothing.
- One request names at most 16 fields, each at most 64 KiB of UTF-8 with no
  NUL, 128 KiB in all. The wire exchange is specified in
  [`credentials.rs`](../crates/phux-protocol/src/credentials.rs).

For SSH keys use [1Password's agent](./ssh.md#keys-in-an-agent), not this.

## Managed by an organization

An organization's control plane can manage a host's credentials instead of
pairing each device:

```sh
phux workload sync-managed --org acme credentials.json
```

The document's format is in the
[CLI reference](./reference/cli.md#phux-workload-sync-managed). It replaces
everything that organization managed before, never touching other
credentials; a dropped credential's connections end. A certificate that
doesn't chain to a listed root refuses the whole document. A managed
credential connects without a pairing token, so the registry is the only
authority: a certificate under the root with no registry entry is refused.
Feed it only documents from a control plane you trust. `phux workload list`
shows each credential's owner and organization.
