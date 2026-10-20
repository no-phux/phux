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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Non-empty value disables the TUI release check and its toast.
///
/// Any non-empty value is an opt-out (`phux-tui`'s `update_notice`). The
/// harness sets [`NO_UPDATE_CHECK_VALUE`] on every spawned `phux` so a
/// published release cannot paint over a scripted attach and consume the
/// next key.
pub const NO_UPDATE_CHECK_ENV: &str = "PHUX_NO_UPDATE_CHECK";

/// The value [`disable_update_check`] writes. Any non-empty value opts out.
pub const NO_UPDATE_CHECK_VALUE: &str = "1";

/// `PHUX_*` keys the test harness itself sets for every child (`just e2e`'s
/// auto-spawn idle backstop, and the release-check opt-out), which must
/// survive the scrub.
pub const HARNESS_PHUX_KEYS: &[&str] = &["PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE", NO_UPDATE_CHECK_ENV];

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

/// `Command::new(bin)` with [`scrub_ambient_phux`] already applied and
/// `XDG_STATE_HOME` / `XDG_CONFIG_HOME` pointed at this test process's
/// [`isolated_xdg`] directories. A caller that sets either key afterwards
/// wins; `env_clear` drops both and drops [`NO_UPDATE_CHECK_ENV`]. Call
/// [`disable_update_check`] again after `env_clear`.
pub fn phux_cmd(bin: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(bin);
    scrub_ambient_phux(&mut cmd);
    let xdg = isolated_xdg();
    cmd.env("XDG_STATE_HOME", &xdg.state)
        .env("XDG_CONFIG_HOME", &xdg.config);
    disable_update_check(&mut cmd);
    cmd
}

/// Turn the TUI release check and its toast off for this child.
///
/// Set this after `env_clear`. A private `XDG_STATE_HOME` is not enough: the
/// attach still looks up the latest release and a cached answer still paints.
pub fn disable_update_check(cmd: &mut Command) {
    cmd.env(NO_UPDATE_CHECK_ENV, NO_UPDATE_CHECK_VALUE);
}

/// A private `XDG_STATE_HOME` and `XDG_CONFIG_HOME` pair under the cargo
/// target's temp directory, so a test's server never opens the developer's
/// organization store or config.
#[derive(Debug, Clone)]
pub struct XdgDirs {
    /// `XDG_STATE_HOME`.
    pub state: PathBuf,
    /// `XDG_CONFIG_HOME`.
    pub config: PathBuf,
}

impl XdgDirs {
    /// A fresh, empty pair under the target temp directory, named by `tag`,
    /// the process id, and a counter.
    pub fn fresh(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = xdg_root().join(format!("{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Self {
            state: root.join("state"),
            config: root.join("config"),
        };
        std::fs::create_dir_all(&dirs.state).expect("create isolated XDG_STATE_HOME");
        std::fs::create_dir_all(&dirs.config).expect("create isolated XDG_CONFIG_HOME");
        dirs
    }

    /// Point `cmd` at this pair.
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env("XDG_STATE_HOME", &self.state)
            .env("XDG_CONFIG_HOME", &self.config);
    }
}

/// The pair [`phux_cmd`] uses: one per test process, which under nextest is
/// one per test.
pub fn isolated_xdg() -> &'static XdgDirs {
    static DIRS: OnceLock<XdgDirs> = OnceLock::new();
    DIRS.get_or_init(|| XdgDirs::fresh("proc"))
}

fn xdg_root() -> PathBuf {
    option_env!("CARGO_TARGET_TMPDIR")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("phux-test-xdg")
}

/// Run the crate's `phux` binary with `args` under `XDG_CONFIG_HOME` and
/// return `(exit_code, stdout, stderr)`.
///
/// Four CLI suites used to copy this helper across three test binaries
/// (phux-n0du). Suites that strip `dhat:` lines or set extra env keep their
/// own wrappers.
pub fn run_with_xdg(args: &[&str], xdg_config_home: &Path) -> (i32, String, String) {
    let out = phux_cmd(crate::runner::phux_bin())
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
            super::NO_UPDATE_CHECK_ENV,
        ] {
            assert!(!super::is_ambient_phux_key(OsStr::new(key)), "{key}");
        }
    }

    #[test]
    fn phux_cmd_disables_the_release_check() {
        let cmd = super::phux_cmd("phux");
        let value = cmd
            .get_envs()
            .find_map(|(key, value)| (key == super::NO_UPDATE_CHECK_ENV).then_some(value));
        assert_eq!(
            value,
            Some(Some(OsStr::new(super::NO_UPDATE_CHECK_VALUE))),
            "every harness-spawned phux must opt out of the release check"
        );
    }
}
