//! Default Unix-domain-socket path resolution.
//!
//! Lives in `phux-config` (not `phux-server`) so thin consumers — the MCP
//! adapter, CLI verbs, future satellites — can agree with the daemon on one
//! socket location without pulling in the heavy server crate (phux-93b).

use std::path::PathBuf;

/// Resolve the default Unix-domain-socket path.
///
/// Precedence (the daemon binds this; every consumer connects to it):
/// 1. `$PHUX_SOCKET` if set — an explicit `--socket` flag still overrides it
///    at the call sites that take one;
/// 2. `$XDG_RUNTIME_DIR/phux/phux.sock` if `XDG_RUNTIME_DIR` is set;
/// 3. `/tmp/phux-$UID/phux.sock` otherwise.
#[must_use]
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PHUX_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let mut p = PathBuf::from(dir);
        p.push("phux");
        p.push("phux.sock");
        return p;
    }
    // SAFETY-free: `getuid` is a `libc` call we'd rather not depend on here.
    // Reading the effective UID from `/proc` is Linux-only; instead use the
    // `USER` env var as a stable, portable fallback when crafting the path.
    // The exact directory name is cosmetic — it only needs to be unique per
    // user.
    let uid_segment = std::env::var("UID")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "default".to_owned());
    let mut p = PathBuf::from("/tmp");
    p.push(format!("phux-{uid_segment}"));
    p.push("phux.sock");
    p
}
