# 1Password credentials

Keep API credentials in 1Password and supply them to a local or remote program
only when you approve its request. phux stores references, not a vault password,
Secret Key, service-account token, or a copy of the credentials.

There are three participants: the program asking for credentials, a phux client
where you review the request, and a provider running beside your 1Password
desktop app. All three connect to the **same phux server**. The provider can be
on a different machine from that server or program.

## Set up a provider

Install the official [1Password CLI](https://developer.1password.com/docs/cli/)
on the device with the 1Password desktop app. In 1Password's **Settings →
Developer**, enable **Integrate with 1Password CLI**. Enable Touch ID, Windows
Hello, or system authentication in 1Password if available.

Start the provider on that device, choosing a name and optionally an account:

```sh
phux credentials serve --name laptop --account my.1password.com
```

For programs running on another machine, connect the provider to that machine's
phux server instead:

```sh
phux credentials serve --name laptop --account my.1password.com --remote build-box
```

`--remote` uses an already configured phux remote. It does not copy your vault
or start `op` on the remote machine. The provider runs until interrupted; it
does not install a background service or change 1Password settings.

Provider names are labels, not authenticated host identities. A connected name
cannot be taken over. Each request binds to the provider's actual connection
and shows its client and authenticated credential when present. A disconnect
ends that binding and fails its outstanding requests; no request transfers to
a replacement provider.

## Run a program with approved credentials

Copy a field's **Secret Reference** from 1Password. Run this on the machine
where the program should execute:

```sh
phux credentials run --provider laptop \
  --env OPENAI_API_KEY=op://Development/OpenAI/credential \
  --purpose 'Coding agent for the API project' -- codex
```

Repeat `--env` for more variables. References may use vault/item/field IDs or
names with ASCII letters, digits, spaces, dash, underscore and dot. A section
adds one path component. Templates, query parameters, and private-key export
modifiers are not accepted.

`run` always executes the command **locally**, including with `--remote`.
The remote option chooses the broker, not an execution host. Start `run` in a
remote shell to inject into a program there.

Review the pending request in the connected client's approval view, or:

```sh
phux approvals
phux approve REQUEST_ID
# Or refuse it:
phux deny REQUEST_ID
```

Check the exact references, provider identity, and requester. The purpose is
provided by the requester; it is not verified process identity. Approval
allows this request once, not future requests. The provider then invokes
`op read` for those exact references. 1Password handles any native unlock
prompt on the provider device. The program starts only after every field
resolves. Values enter its environment, never its command-line arguments or
a terminal paste. The program's exit status is preserved.

## What approval does and does not mean

- Phux approval and 1Password authentication are separate. Every exchange
  needs a phux approval, even on the owner socket. 1Password may reuse its
  own authenticated session; phux does not promise a biometric prompt for
  every field or request.
- A phone or browser can decide a phux request but cannot unlock a desktop's
  1Password using the phone's biometrics. The official desktop integration
  requires authentication on the provider device. A locked or unavailable
  provider fails closed; phux never asks an agent for your vault password.
- Requesting, providing, and deciding require full `BIND` and `INPUT` on
  Global. A pane-scoped delegated agent key cannot request or release
  credentials. Like the owner socket elsewhere in phux, this is **not a
  sandbox against programs running as your OS user**. Approval APIs do not
  prove physical human presence.
- The broker is trusted with delivered plaintext in memory. Remote links use
  phux's authenticated transport; this is not encryption hiding values from
  the phux server. Do not use an untrusted server as a broker.
- Values never enter approval metadata, the event journal, replay receipts,
  or provider diagnostics. Secret value Debug output is redacted and owned
  value buffers are erased on drop where possible. This is not a guarantee
  against OS inspection, swap, crash dumps or copies in transport buffers.
- The receiving program can read and leak its environment. Do not run an
  untrusted program or print its environment. phux cannot redact arbitrary
  output from the child. Credentials remain usable by the child until it
  exits; denying a later request does not revoke an issued API key.
- A request expires within 120 seconds (or the shorter configured approval
  TTL), including unlock time. Denial, disconnect, timeout, invalid values
  and provider failure release nothing. Unknown outcomes are not retried.
- At most 16 fields, 64 KiB per field and 128 KiB total are delivered per
  request. Values must be UTF-8 without NUL for environment injection.

## SSH keys

For SSH authentication, prefer [1Password's SSH agent through your SSH
configuration](./ssh.md#keys-in-an-agent); phux should not export a private
key into an environment variable. This credential broker is not SSH-agent
or arbitrary socket forwarding.

## Wire contract

`CREDENTIALS` (command tag `0x4e`, `features_ext.CREDENTIALS`, mask `0x00800000`)
is an ephemeral exchange. Its body is length-prefixed UTF-8 JSON, at most
1 MiB, matching `phux_protocol::credentials::CredentialOp`: `request` with
provider, references and purpose; `next` with provider; or `complete` with
exchange id and nullable values. All require `BIND` and `INPUT` on Global.
`next` binds a provider name to its connection and claims at most one approved
exchange (`OK` if none, otherwise JSON `{id,request,requester}`).

Every `request`, including on the owner socket, opens an ordinary approval
record with additive `credential: {request,provider}` details. The provider
is bound before approval. Only that connection may claim and complete it.
The original request receives `CommandValue` tag `0x0e`, length-prefixed
JSON containing an ordered string array, once. Values are redacted in Debug,
never journaled or cached in keyed receipts. The full exchange expires within
120 seconds or the shorter approval TTL. Disconnects withdraw it; requests,
claims and completions are never replayed. Clients must check the feature bit.

Credential approvers must offer HELLO field 12 `CREDENTIAL_APPROVALS`
(`u8`, exactly `1`; absent/other = false), declaring that they review the exact
references and bound provider. Legacy clients may deny but receive
`UNSUPPORTED` if they try to approve an opaque credential request.

## Provider troubleshooting

Both client and server need credential support. Clients check the server's
`CREDENTIALS` feature; older servers are refused before a credential command
is sent. If no provider is connected,
start `serve` against the same server as `run`.

An older client that cannot show credential details may deny a request but
cannot approve it. Upgrade the reviewing client if phux reports that it
cannot review the 1Password references and bound provider.

On provider failures, check the CLI installation and app integration locally.
Provider stderr is deliberately discarded because it may contain sensitive
data. phux removes inherited `OP_*` authentication settings and enables the
desktop integration explicitly, rather than falling back to a service token.

See 1Password's [app integration setup](https://developer.1password.com/docs/cli/app-integration/)
and [authentication security model](https://developer.1password.com/docs/cli/app-integration-security/).
Those APIs keep the unlock credential inside 1Password; phux does not implement
or simulate its biometric prompt.
