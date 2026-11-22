# Host-scoped read keys

Give each CI runner host a certificate that can list sessions on this
server and cannot read a terminal or type into one. Give a collaborator's
device a certificate that can list the same sessions and can read only
the sessions the owner opts in, one session at a time. Neither certificate
can type. The certificate is the existing `phux workload add-key`
enrollment. There is no separate grant type.

Mint these on the serving host, as its owner. One certificate per runner
host, and one for each collaborator's device. `ci-runner-1` is a placeholder.

## What the scope is

A CI runner lists this host and nothing more:

```sh
--scope inventory@host
```

The collaborator lists this host, and reads each session the owner chooses to
share. Each session is opted in explicitly, as its own scope:

```sh
--scope inventory@host \
--scope observe@session:KEY
```

`host` is this serving host. `host:NAME` names a satellite of this server,
not the server itself. `session:KEY` names one session. `KEY` is that
session's full key: 26 lowercase base32 digits, the `key` field of its
row in `phux ls --json` (run that on the owner socket). The short `^`
prefix on the human listing is not a selector. `--scope` is repeatable,
one `<verb>[,<verb>...]@<selector>` per flag. Add another
`--scope observe@session:KEY` for every further session. A session left
off the command can still be listed and cannot be read.

`observe@host` is screen and scrollback of every terminal on the host,
secrets included. This runbook does not grant `observe@host` or
`inventory,observe@host`.

`phux ls` sends `GET_STATE { SERVER }`. That method is admitted only with
`inventory`, and the snapshot keeps what `inventory` covers, so a
certificate whose only verb is `observe` connects and can run `phux whoami`
but `phux ls --all` reports the host unreachable. `inventory` is what
lists. `observe` on a `session:` selector is what reads that session's
terminals (`GET_SCREEN`), and only those. Do not add `input`, `create`,
`bind`, `signal`, `organize`, `terminate`, or `*`.

`--principal` is the kind of holder, not a username:

| Holder | `--principal` | `--expires-in` |
| --- | --- | --- |
| CI runner host `ci-runner-1` | `automation` | `604800` (7 days) |
| Collaborator's device | `human` | `2592000` (30 days) |

Every `add-key` below passes `--expires-in`. The flag's default is 90
days (`7776000` seconds), longer than either holder should keep a
certificate, which is why the lifetime is written out.

Record the printed `sha256:` credential id next to the holder
(`ci-runner-1`, the collaborator's device) in the operator's own notes. The
registry does not store that label.

`phux pair --expires` is a different knob, on the pairing bearer, and it
refuses a duration longer than 30 days. `add-key --expires-in` is the
certificate lifetime in seconds. Its ceiling is 20 years (`630720000`);
these certificates stay at 7 or 30 days so the certificate and the bearer
rotate on the same calendar.

## Mint

The client creates a key and a certificate signing request. Only the
request is pasted here. phux refuses input that contains a private key.

On each serving host:

```sh
phux workload authority --init
```

`--init` creates the CA when this host has none. An existing CA is left
alone. The printed `sha256:` fingerprint is what clients pin.

CI runner `ci-runner-1`, seven days, automation. Inventory only:

```sh
phux workload add-key \
  --scope inventory@host \
  --expires-in 604800 \
  --principal automation \
  --json \
  --cert-stdout \
  < /path/to/ci-runner-1.csr
```

Collaborator's device, 30 days, human. `KEY` is one session's full key.
Repeat the `observe@session:` line for each session the owner shares:

```sh
phux workload add-key \
  --scope inventory@host \
  --scope observe@session:KEY \
  --expires-in 2592000 \
  --principal human \
  --json \
  --cert-stdout \
  < /path/to/collaborator.csr
```

Stdout is one JSON document. `credential_id` is the `sha256:` id.
`certificate_chain` is the PEM (leaf, then the CA) to put in the client's
`PHUX_WORKLOAD_CERT`. The private key stays in the client's
`PHUX_WORKLOAD_KEY` and never comes back to this host.

Repeat the CI command once per runner host, each with its own request and
its own recorded id. Do not reuse one certificate across runners.

Do not enroll these holders with `phux host add user@host`. That command
asks the far host for the paired-device ceiling, which includes `input`.

## Let the client list

A routable listener still requires a pairing bearer. The bearer admits the
connection. It does not grant scope; the certificate does. On the serving
host, `phux pair` prints a token and the server fingerprint. `phux pair
--expires 30d` is the longest bearer that command will mint.

On the client, register the host with placeholders, then list:

```sh
phux host add placeholder-host quic://placeholder-host.example:8788 \
  --token-file /path/to/placeholder-host.token \
  --cert-fingerprint sha256:FINGERPRINT

export PHUX_WORKLOAD_CERT=/path/to/ci-runner-1.pem
export PHUX_WORKLOAD_KEY=/path/to/ci-runner-1.key
phux ls --all
```

`phux download` and anything else that needs `input` is refused with
permission denied. So is `GET_SCREEN` on the CI certificate: that command
is `observe` of a terminal, and the runner's scope has no `observe`.
Listing the local owner socket does not exercise this certificate; `phux
ls --all` does, on the remote row.

## Rotate

Make a new request and use the recorded predecessor id. `--replace`
revokes that id, when present and active, in the same registry write.
Pass the same scopes the holder already has, and pass `--expires-in` again.

CI runner, seven days:

```sh
phux workload add-key \
  --scope inventory@host \
  --expires-in 604800 \
  --principal automation \
  --replace sha256:PREVIOUS \
  --json \
  --cert-stdout \
  < /path/to/ci-runner-1-next.csr
```

Collaborator, 30 days, with each session still shared named again:

```sh
phux workload add-key \
  --scope inventory@host \
  --scope observe@session:KEY \
  --expires-in 2592000 \
  --principal human \
  --replace sha256:PREVIOUS \
  --json \
  --cert-stdout \
  < /path/to/collaborator-next.csr
```

Before distributing the new chain, require `replaced: true` in the JSON
response and verify the recorded predecessor is revoked in `phux workload
list`. An unknown canonical id is accepted with `replaced: false`; command
success alone does not prove the intended old certificate was withdrawn.
If that check fails, find and revoke the actual predecessor before
completing the rotation.

A confirmed replacement revokes the predecessor immediately, including
its live connections. Install the new chain and key on that client to
reconnect; do not expect an overlap with the old certificate.

Renew an expiring pairing bearer separately: mint a fresh bounded-lifetime
credential with `phux pair --expires 7d` (or at most `30d`), distribute its
token to the client, then revoke the previous pairing credential id with
`phux pair revoke PAIRING_ID`. This does not change the certificate scope.
`phux pair rotate PAIRING_ID` rotates only the secret with a bounded
overlap; it preserves the credential's absolute expiry, so it is not a
lifetime renewal.

## Revoke

```sh
phux workload revoke sha256:CREDENTIAL
```

A running server watches the registry and closes every live connection
that certificate admitted. No restart. `phux pair revoke` of the same
`sha256:` id does the same for the certificate. Revoking the pairing
bearer also stops new dials and ends live connections admitted by that
bearer; its expiry ends those connections too. Revoke the certificate to
withdraw that holder's scope regardless of which bearer it used.

`phux workload list` shows the id, the scope, the principal, and whether
it is still active.
