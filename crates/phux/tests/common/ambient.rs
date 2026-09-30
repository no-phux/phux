//! Shared `PHUX_*` scrub for binaries that spawn `CARGO_BIN_EXE_phux`
//! (phux-lru0). `common/mod.rs` re-exports it, so the full server-process
//! harness scrubs too.

#![allow(
    dead_code,
    reason = "shared integration-test helpers are used per test binary"
)]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(clippy::expect_used, reason = "test harness")]

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

/// Keys a live pane of the production server exports, kept as the named
/// floor of [`ambient_phux_keys`]: a child spawned from `CARGO_BIN_EXE_phux`
/// inherits them unless they are removed, and they override per-test XDG
/// tempdirs, including minting into the operator token store (phux-lru0).
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

/// `PHUX_*` keys the test harness itself sets for every child (`just e2e`'s
/// auto-spawn idle backstop), which must survive the scrub.
pub const HARNESS_PHUX_KEYS: &[&str] = &["PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE"];

/// Whether an inherited variable is phux process state. Every `PHUX_*` key
/// but the harness's own is, so a path variable added later (workload
/// material, upload and install dirs, `PHUX_PROFILE`, `PHUX_TERMINAL_ID`) is
/// scrubbed without anyone remembering to list it.
pub fn is_ambient_phux_key(key: &OsStr) -> bool {
    key.as_encoded_bytes().starts_with(b"PHUX_")
        && !HARNESS_PHUX_KEYS.iter().any(|harness| key == *harness)
}

/// The `PHUX_*` keys present in this test process's environment, plus the
/// named floor. For spawners without `env_remove` on a `Command`
/// (`portable_pty::CommandBuilder`).
pub fn ambient_phux_keys() -> Vec<OsString> {
    let mut keys: Vec<OsString> = AMBIENT_PHUX_KEYS.iter().map(OsString::from).collect();
    for (key, _) in std::env::vars_os() {
        if is_ambient_phux_key(&key) && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// Drop inherited `PHUX_*` process state. Callers then set only the
/// isolation table they mean. `PATH` and XDG are left alone.
pub fn scrub_ambient_phux(cmd: &mut Command) {
    for key in ambient_phux_keys() {
        cmd.env_remove(key);
    }
}

/// `Command::new(bin)` with [`scrub_ambient_phux`] already applied.
pub fn phux_cmd(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    scrub_ambient_phux(&mut cmd);
    cmd
}

/// Run the crate's `phux` binary with `args` under `XDG_CONFIG_HOME` and
/// return `(exit_code, stdout, stderr)`.
///
/// Four CLI suites used to copy this helper across three test binaries
/// (phux-n0du). Suites that strip `dhat:` lines or set extra env keep their
/// own wrappers.
pub fn run_with_xdg(args: &[&str], xdg_config_home: &Path) -> (i32, String, String) {
    let out = phux_cmd(env!("CARGO_BIN_EXE_phux"))
        .env("XDG_CONFIG_HOME", xdg_config_home)
        .args(args)
        .output()
        .expect("run phux binary");
    (
        out.status.code().expect("phux exited via code, not signal"),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

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
                super::ambient_phux_keys().contains(&key.into()),
                "{key} must be scrubbed so a suite run from a live pane cannot \
                 touch the operator store (phux-lru0)"
            );
        }
    }

    #[test]
    fn every_phux_prefixed_key_is_ambient() {
        for key in ["PHUX_WORKLOAD_CA_KEY", "PHUX_PROFILE", "PHUX_UPLOAD_DIR"] {
            assert!(super::is_ambient_phux_key(OsStr::new(key)), "{key}");
        }
        for key in [
            "HOME",
            "XDG_STATE_HOME",
            "MY_PHUX_THING",
            "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE",
        ] {
            assert!(!super::is_ambient_phux_key(OsStr::new(key)), "{key}");
        }
    }
}
