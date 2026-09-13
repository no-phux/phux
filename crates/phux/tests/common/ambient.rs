//! Shared `PHUX_*` scrub for binaries that spawn `CARGO_BIN_EXE_phux`
//! without the full server-process harness (phux-lru0).

#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]

use std::ffi::OsStr;
use std::process::Command;

/// Process-manager `PHUX_*` keys a live pane exports. A child spawned from
/// `CARGO_BIN_EXE_phux` inherits them unless they are removed, and they
/// override per-test XDG tempdirs — including minting into the operator
/// token store (phux-lru0).
pub const AMBIENT_PHUX_KEYS: &[&str] = &[
    "PHUX_SOCKET",
    "PHUX_WS_ADDR",
    "PHUX_WS_SECURE",
    "PHUX_WS_TOKENS",
    "PHUX_WS_TLS_CERT",
    "PHUX_WS_TLS_KEY",
    "PHUX_QUIC_ADDR",
    "PHUX_WT_ADDR",
    "PHUX_SERVICE_MANAGED",
    "PHUX_TAILSCALE",
    "PHUX_LOG",
];

/// Drop inherited `PHUX_*` process state. Callers then set only the
/// isolation table they mean. `PATH` and XDG are left alone.
pub fn scrub_ambient_phux(cmd: &mut Command) {
    for key in AMBIENT_PHUX_KEYS {
        cmd.env_remove(*key);
    }
}

/// `Command::new(bin)` with [`scrub_ambient_phux`] already applied.
pub fn phux_cmd(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    scrub_ambient_phux(&mut cmd);
    cmd
}

#[cfg(test)]
mod tests {
    #[test]
    fn ambient_phux_keys_cover_the_service_manager_exports() {
        for key in [
            "PHUX_SOCKET",
            "PHUX_WS_TOKENS",
            "PHUX_WS_TLS_CERT",
            "PHUX_WS_TLS_KEY",
            "PHUX_SERVICE_MANAGED",
        ] {
            assert!(
                super::AMBIENT_PHUX_KEYS.contains(&key),
                "{key} must be scrubbed so a suite run from a live pane cannot \
                 touch the operator store (phux-lru0)"
            );
        }
    }
}
