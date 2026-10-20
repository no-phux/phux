# Host-scoped read keys

Give each CI runner host a certificate that can list sessions on this
server and cannot read a terminal or type into one. Give jamesponwith's
device a certificate that can list the same sessions and can read only
the sessions Patrick opts in, one session at a time. Neither certificate
can type. The certificate is the existing `phux workload add-key`
enrollment. There is no separate grant type.

Mint these on the serving host, as its owner. One certificate per runner
host, and one for jamesponwith's device. `ci-runner-1` is a placeholder.

## What the scope is

A CI runner lists this host and nothing more:

```sh
--scope inventory@host
```

jamesponwith lists this host, and reads each session Patrick chooses to
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
| jamesponwith's device | `human` | `2592000` (30 days) |

Every `add-key` below passes `--expires-in`. The flag's default is 90
days (`7776000` seconds), longer than either holder should keep a
certificate, which is why the lifetime is written out.

Record the printed `sha256:` credential id next to the holder
(`ci-runner-1`, jamesponwith's device) in the operator's own notes. The
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

jamesponwith's device, 30 days, human. `KEY` is one session's full key.
Repeat the `observe@session:` line for each session Patrick shares:

```sh
phux workload add-key \
  --scope inventory@host \
  --scope observe@session:KEY \
  --expires-in 2592000 \
  --principal human \
  --json \
  --cert-stdout \
  < /path/to/jamesponwith.csr
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

Make a new request. `--replace` revokes the previous id in the same
registry write, so the client is never enrolled twice. Pass the same
scopes the holder already has, and pass `--expires-in` again.

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

jamesponwith, 30 days, with each session still shared named again:

```sh
phux workload add-key \
  --scope inventory@host \
  --scope observe@session:KEY \
  --expires-in 2592000 \
  --principal human \
  --replace sha256:PREVIOUS \
  --json \
  --cert-stdout \
  < /path/to/jamesponwith-next.csr
```

Install the new chain and key on that client before the old certificate
expires. Rotate the pairing bearer separately with `phux pair rotate` when
its own expiry is near; that does not change the certificate scope.

## Revoke

```sh
phux workload revoke sha256:CREDENTIAL
```

A running server watches the registry and closes every live connection
that certificate admitted. No restart. `phux pair revoke` of the same
`sha256:` id does the same for the certificate. Revoking the pairing
bearer stops new dials that present it; revoke the certificate to end
connections that are already open on it.

`phux workload list` shows the id, the scope, the principal, and whether
it is still active.
