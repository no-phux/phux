//! The private environment the remote-dial lifecycle suites run under:
//! their own config, state, and runtime dirs, and no ssh or overlay probe.

#![allow(
    unreachable_pub,
    reason = "shared by sibling suites in this test binary"
)]
#![allow(clippy::expect_used, reason = "tests")]

use std::path::{Path, PathBuf};

/// The environment every process in a hermetic test runs under.
pub fn hermetic_env(dir: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("XDG_CONFIG_HOME", dir.join("config")),
        ("XDG_STATE_HOME", dir.join("state")),
        ("XDG_RUNTIME_DIR", dir.join("run")),
        ("PHUX_PROFILE", PathBuf::from("default")),
        // Any ssh or overlay probe fails loudly instead of reaching out.
        ("PHUX_SSH", dir.join("no-such-ssh")),
        ("PHUX_TAILSCALE", dir.join("no-such-tailscale")),
    ]
}

/// Create the config and runtime dirs [`hermetic_env`] points at.
pub fn prepare_dirs(dir: &Path) {
    std::fs::create_dir_all(dir.join("config/phux")).expect("config dir");
    std::fs::create_dir_all(dir.join("run")).expect("runtime dir");
}
