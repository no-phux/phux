//! Filesystem locations for relay state.
//!
//! The relay keeps exactly three files, all in the phux state directory:
//! `relay-tokens`, `relay-cert.pem`, and `relay-key.pem` — siblings of the
//! server's `remote-tokens` / `remote-cert.pem` / `remote-key.pem` so
//! operators find them where they expect.

use std::path::PathBuf;

/// phux's per-user state directory: `$XDG_STATE_HOME/phux` (or
/// `$HOME/.local/state/phux` when `XDG_STATE_HOME` is unset/empty).
///
/// Deliberate small duplication of `phux_server::telemetry::state_dir`:
/// depending on `phux-server` for eight lines would drag the entire daemon
/// graph (libghostty included) into the relay build, and the relay must
/// share zero code with the daemon (ADR-0051 invariant 4).
#[must_use]
pub fn state_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || {
                let mut home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
                home.push(".local");
                home.push("state");
                home
            },
            PathBuf::from,
        );
    base.join("phux")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_ends_with_phux() {
        assert_eq!(
            state_dir().file_name().and_then(|n| n.to_str()),
            Some("phux")
        );
    }
}
