# Host-scoped read keys

Give each CI runner host, and one other person's device, a certificate
that can list sessions on this server and cannot type into them. The
certificate is the existing `phux workload add-key` enrollment. There is
no separate grant type.

Mint these on the serving host, as its owner. One certificate per runner
host, and one for the other person's device. The examples use placeholder
names only.

## What the scope is

```sh
--scope inventory,observe@host
```

`host` is this serving host. `host:NAME` names a satellite of this server,
not the server itself.

`phux ls` sends `GET_STATE { SERVER }`. That method is admitted only with
`inventory`, and the snapshot keeps what `inventory` covers, so a
certificate whose only verb is `observe` connects and can run `phux whoami`
but `phux ls --all` reports the host unreachable. `observe` is included so
the same certificate can read a terminal it may list. Do not add `input`,
`create`, `bind`, `signal`, `organize`, `terminate`, or `*`.

`--principal` is the kind of holder, not a username:

| Holder | `--principal` | Suggested `--expires-in` |
| --- | --- | --- |
| CI runner host `ci-runner-1` | `automation` | `604800` (7 days) |
| Other member's device `member-device` | `human` | `2592000` (30 days) |

Record the printed `sha256:` credential id next to the placeholder
(`ci-runner-1`, `member-device`) in the operator's own notes. The registry
does not store that label.

`phux pair --expires` is a different knob, on the pairing bearer, and it
refuses a duration longer than 30 days. `add-key --expires-in` is the
certificate lifetime. Its default is 90 days and its ceiling is 20 years;
stay at or under 30 days so the certificate and the bearer rotate on the
same calendar.

## Mint

The client creates a key and a certificate signing request. Only the
request is pasted here. phux refuses input that contains a private key.

On each serving host:

```sh
phux workload authority --init
```

`--init` creates the CA when this host has none. An existing CA is left
alone. The printed `sha256:` fingerprint is what clients pin.

CI runner `ci-runner-1`, seven days, automation:

```sh
phux workload add-key \
  --scope inventory,observe@host \
  --expires-in 604800 \
  --principal automation \
  --json \
  --cert-stdout \
  < /path/to/ci-runner-1.csr
```

Other member's device `member-device`, 30 days, human:

```sh
phux workload add-key \
  --scope inventory,observe@host \
  --expires-in 2592000 \
  --principal human \
  --json \
  --cert-stdout \
  < /path/to/member-device.csr
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
permission denied. Listing the local owner socket does not exercise this
certificate; `phux ls --all` does, on the remote row.

## Rotate

Make a new request. `--replace` revokes the previous id in the same
registry write, so the client is never enrolled twice:

```sh
phux workload add-key \
  --scope inventory,observe@host \
  --expires-in 604800 \
  --principal automation \
  --replace sha256:PREVIOUS \
  --json \
  --cert-stdout \
  < /path/to/ci-runner-1-next.csr
```

Replacement revokes the predecessor immediately, including its live
connections. Install the new chain and key on that client to reconnect;
do not expect an overlap with the old certificate.

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
