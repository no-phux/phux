//! Default UDS path resolution for the MCP adapter.
//!
//! Delegates to `phux_config::socket::default_socket_path`, the same
//! resolver the daemon binds (phux-server re-exports it), so the daemon
//! and this thin adapter agree on one definition without the adapter
//! depending on the heavy server crate (phux-93b).

use std::path::PathBuf;

/// Resolve the UDS path a tool should connect to.
///
/// An explicit `socket` argument (the tool's optional `socket` field) wins;
/// otherwise the shared default applies — `$PHUX_SOCKET`, then
/// `$XDG_RUNTIME_DIR/phux/phux.sock`, then `/tmp/phux-$USER/phux.sock`.
#[must_use]
pub(crate) fn resolve(explicit: Option<&str>) -> PathBuf {
    explicit.map_or_else(phux_config::socket::default_socket_path, PathBuf::from)
}
