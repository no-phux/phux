# portable-pty-adopt

Rebuild [`portable-pty`](https://crates.io/crates/portable-pty)
`MasterPty` / `Child` trait objects from an **inherited** PTY master file
descriptor and a child process id.

## Why

`portable-pty` constructs a `MasterPty`/`Child` only by *creating* the PTY —
`openpty()` mints a fresh pair, `spawn_command()` forks the child. The concrete
Unix types are private and offer no `from_raw_fd` / `from_pid` constructor. So
once a PTY exists you cannot rebuild those trait objects from the raw
`(master_fd, child_pid)` you can read back via `MasterPty::as_raw_fd()` and
`Child::process_id()`.

That blocks **graceful, in-place restarts**. The standard reload primitive
(nginx, HAProxy, systemd socket activation) clears `FD_CLOEXEC` on the
descriptors to keep and `execve`s the new binary: open fds survive the exec,
and since `execve` preserves the process identity, the children stay alive and
stay *yours* (`waitpid` keeps working). The new image inherits each PTY master
as a bare fd and learns the child PID from a handoff blob — but with
`portable-pty` alone it cannot turn those back into the `MasterPty`/`Child` its
plumbing is written against.

`AdoptedMaster` and `AdoptedChild` are that missing constructor.

```rust
use portable_pty_adopt::{AdoptedChild, AdoptedMaster};
use std::os::fd::OwnedFd;

// In the resumed process, after execve, with `master_fd` inherited
// (FD_CLOEXEC cleared before exec) and `child_pid` from the handoff blob:
let master: Box<dyn portable_pty::MasterPty + Send> =
    Box::new(unsafe { AdoptedMaster::from_raw_fd(master_fd) });
let child: Box<dyn portable_pty::Child + Send + Sync> =
    Box::new(AdoptedChild::new(child_pid));
// `master`/`child` now drop into the same code paths you use for a
// freshly-spawned PTY: read/write/resize the master, try_wait/kill the child.
```

## Scope

- **Unix only** (PTYs, `waitpid`, `tcgetpgrp` are POSIX).
- You must **own** the master fd (it is closed on drop) and be the child's
  **parent** (true across `execve`, not across `fork`).
- `Child::kill` sends `SIGKILL`, matching `portable-pty`'s own implementation.

## Status

> **TODO(extract).** This crate currently lives inside the
> [phux](https://github.com/no-phux/phux) workspace, where it backs phux's
> graceful server upgrade (ADR-0032). It has no phux-specific dependencies and
> is meant to be split into its own repository and published to crates.io.
> Until then it tracks the `portable-pty` version pinned by that workspace.

License: Apache-2.0
