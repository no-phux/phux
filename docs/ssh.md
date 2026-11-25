---
group: connect
order: 1
---

# SSH

How phux uses your own `ssh` (config, jump hosts, keys) to add a machine,
attach without pairing, and forward ports and your ssh-agent.

## Signing in once

From a terminal, phux logs in to a host once, so a password, 2FA, or
security-key prompt appears a single time and later steps reuse the login.
It keeps that login as its own ssh connection in `/tmp/phux-ssh-<uid>`
(owner-only) until five minutes after last use; your own `ControlMaster` and
`LocalForward` settings are not touched. Rune asks in a dialog instead (on
Linux, through `SSH_ASKPASS`). From a script phux never prompts, so the host
needs a key that works alone. With `BatchMode yes` in your config, ssh can't
ask for a password, so neither can phux; the error says so.

Logins turned away by sshd's `MaxStartups` are retried a few times; a
refused password, key, or host key never is. Past `MaxSessions`, ssh logs
the extra connection in on its own, which asks again on a password host.
When ssh fails with little explanation, phux prints the same `ssh` command
with `-v` to copy.

## When the host key changes

phux stops and prints the `ssh-keygen -R` command that removes the old key.
A reinstall changes it, and so does someone intercepting the connection:
confirm the new key with whoever runs the host first. phux never removes or
accepts a key for you.

## Keys in an agent

phux's `ssh` finds your agent through `IdentityAgent` in `~/.ssh/config`, or
the inherited `SSH_AUTH_SOCK`. An app opened from the Dock has no
`SSH_AUTH_SOCK`, so in Rune a key held only in an agent (1Password,
Secretive) fails with `Permission denied (publickey)`. Either fixes it, and
`IdentityAgent` wins:

```
# ~/.ssh/config
Host *
    IdentityAgent ~/.1password/agent.sock
```

```toml
# config.toml
[rune]
ssh-agent = "~/.1password/agent.sock"
```

## Keys in hardware

Two keys reach a machine: your ssh key (use an `sk-` key or a hardware agent
to keep it in hardware), and the key behind the client certificate
`phux host add` enrolls. On a Mac with a Secure Enclave, `--hardware-key`
keeps the certificate's key in the enclave:

```sh
phux host add me@mini --hardware-key
phux host add mini --code LINK --hardware-key
phux host renew mini --hardware-key            # move an existing file key
```

The key never leaves the chip, and the key file holds only a reference this
Mac's enclave can use. A copied key file signs nothing elsewhere. It does
not protect against a program running as you on this Mac, which can ask the
enclave to sign without a prompt. Renewal keeps the old key until the server
accepts the new certificate. `phux host ls` shows `hardware` or `file`. On a
machine without an enclave the flag is refused and nothing changes.

## Attaching over SSH

```sh
phux attach --ssh me@box
```

SSH logs you in, starts the remote server if needed, and opens a QUIC
listener for this attach alone; then SSH exits and the session runs over
QUIC, so it survives network changes. The listener and its token live only
in memory and go away shortly after you detach. `--udp-ports 60000-61000`
keeps the listener in a range one firewall rule can cover.

The remote server opens that listener only in `paired` mode, and admits only
a client certificate it enrolled. If it refuses, or UDP can't get through, phux
says why and carries the session through
`ssh` itself. That does not roam: on a network drop phux logs in again and
re-attaches. phux remembers a host whose UDP didn't answer for a day
(`udp-unanswered` in the state directory) and keeps probing UDP in the
background. An SSH login failure, or a host without phux, is an error, not a
fallback.

## Forwarding a port

```sh
phux forward -L 3000:localhost:3000 --remote mini
```

Like `ssh -L`, through the phux connection: listens on `127.0.0.1:3000` here
and carries each connection to `localhost:3000` on `mini`, until Ctrl-C.
Repeat `-L` to forward more than one port; if any can't listen, none does. If
the phux connection drops, open connections close, the listener stays, and
new connections are served once phux reconnects; a cut-off transfer ends
early rather than with an error. There is no `-R`, `-D`, or UDP, and a
forward does not reach a [linked machine](./remote.md#linking-machines)
behind the server.

By default the server dials only its own loopback (`localhost`,
`127.0.0.1`, `::1`). `[forward]` in the server's
[config](./reference/config.md) turns forwarding off or adds exact
`host:port` targets to `allow` (no wildcards), which the server resolves and
dials as its own user. That default limits mistakes, not a determined holder: forwarding
needs `input@global`, which can already run commands there. List a target
only if every holder of that right may reach it.

## ssh-agent

```sh
phux forward --agent --remote mini
```

Like `ssh -A`: while it runs, `ssh` and `git` in panes on `mini` can use the
keys in this machine's agent. Each signature is asked about first:

```text
phux: let mini sign with SHA256:Xr0... (me@laptop)? [y/N]
```

Anything but `y` refuses. Without a terminal every signature is refused.
Listing keys is not asked about. `--yes` signs without asking.

The server must set `[forward] agent = true` (restart to apply). It then
starts every new pane with `SSH_AUTH_SOCK` pointing at an owner-only broker
socket beside its own, **replacing** any agent the server inherited, whether
or not a client is serving. Panes started earlier keep their old value. The
path survives server restarts. The broker passes on only key listing and
signing; adding, removing, locking, and extensions are refused on both
sides, and the client refuses a key its agent does not hold. An unanswered
request fails after 60 s. The newest serving client wins.

**Once a client serves, any process in any pane on that server, run by
anyone who can reach the server's user account, can ask your agent to
sign.** The prompt names the server and key, not what is signed or which
pane asked, so say yes only when you expect a request. `--yes` is exactly
`ssh -A`. Offering needs `input@global`.

The [audit log](./security.md#audit-log) records each forwarded connection
and each signature request, never the data. The wire contract (timeouts,
windows, limits, errors) is
[`phux-protocol/src/forward.rs`](../crates/phux-protocol/src/forward.rs);
tags are in [kinds](./reference/kinds.md).
