# SSH

phux uses your own `ssh`, with your config, jump hosts and keys, to add a
machine ([`phux host add`](./remote.md#adding-a-machine)) and to attach to one
without pairing. This page covers how it signs in, attaches, and recovers.

## Signing in once

From a terminal, phux logs in to the host once, so a password, 2FA or
security-key prompt comes up a single time and every later step reuses that
login. phux keeps the login open as its own ssh connection, in a directory
only you can read (`/tmp/phux-ssh-<uid>`), until five minutes after its last
use. Your own `ControlMaster` and `LocalForward` settings never touch it.

Rune asks in a dialog instead, for the machine you pick (on Linux, through
your desktop's `SSH_ASKPASS`). Cancelling it stops the connection rather than
asking again.

Without either, as from a script, phux never prompts, so the host needs a key
that works on its own. phux keeps that key login open the same way, so the
next connection skips the SSH handshake.

A host block your tools generated, with its own `ControlMaster`,
`ControlPath`, `ControlPersist` or `BatchMode`, works as is: phux's own
options win on the command line. `BatchMode yes` does keep ssh from asking
for a password or code, so phux can't ask either; if such a host refuses
the login, phux says that's why.

## Keys in an agent

phux's `ssh` finds your agent the way any `ssh` does: through
`IdentityAgent` in `~/.ssh/config`, or else the `SSH_AUTH_SOCK` it inherits.
A terminal has that variable; an app opened from the Dock doesn't, so in
Rune a key held only in an agent (1Password, Secretive, a hardware-key
agent) is not offered and the host answers `Permission denied (publickey)`.
Either line fixes it:

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

`IdentityAgent` wins when both are set. When a key is refused and no agent
is in reach, the error names these two settings.

## Many connections at once

Connections to one host from the same app wait for a single login and then
share it. sshd also turns away logins over its `MaxStartups` limit before
they start, which happens when many open at once; phux tries those again a
few times after a short random pause. A refused password, key or host key
is never retried. If every try is turned away, the error names
`MaxStartups`.

sshd also caps the sessions one login carries (`MaxSessions`, 10 by
default). Past that, ssh logs the extra connection in on its own. A key
host needs nothing more. A password or 2FA host asks again: in Rune's
sign-in dialog, or, from a terminal, through `ssh -t`. If that login fails
too, the error names `MaxSessions`.

When ssh fails and says little more than "Connection closed", phux shows
what ssh printed and a command to copy: the same `ssh` with phux's options
and `-v`, so you can see where it stops.

## When the host key changes

If the host's key changed, phux stops and prints the `ssh-keygen -R` command
that removes the old one. A reinstall changes the key, and so does someone
intercepting the connection, so confirm the new key with whoever runs the
host first. phux never removes or accepts a key for you.

## Attaching over SSH

`phux attach --ssh` needs no pairing and no service:

```sh
phux attach --ssh me@box
```

SSH logs you in (password and 2FA prompts work), starts the server on the
remote machine if needed, and opens a QUIC (UDP) listener for this attach
alone. Then SSH exits and the session runs directly over QUIC, so it survives
network changes and draws locally. The listener and its token live only in
memory and go away shortly after you detach.

The remote server's policy must allow the listener, and UDP must reach it.
If it can't, phux says why and keeps your local client, carried through
`ssh` itself, reusing the login above. That doesn't roam: when the network
drops, phux logs in again (prompting once, if the host asks for a password
or 2FA) and re-attaches. An SSH login failure, or a remote machine without
phux, is an error, not a fallback.

`--udp-ports 60000-61000` keeps the listener in a range one firewall rule
can cover.
