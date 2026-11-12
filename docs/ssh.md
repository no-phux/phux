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
