//! A development build refuses production state end to end
//! (`phux_config::production::refuse_dev_on_production_state`).
//!
//! The guard treats a `HOME` outside the temp directory as production, so
//! each case builds a sandbox whose `TMPDIR` is a sibling of its `HOME`:
//! the sandbox's own `HOME` is then production-shaped, and nothing here
//! can reach the operator's real one. The positive controls move `HOME`
//! inside that `TMPDIR`, the layout test harnesses use, and succeed.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

const PHUX: &str = env!("CARGO_BIN_EXE_phux");

struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let root = TempDir::new().expect("sandbox");
        std::fs::create_dir_all(root.path().join("tmp")).unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        Self { root }
    }

    /// The child's temp directory: `HOME` is not inside it.
    fn tmp(&self) -> PathBuf {
        self.root.path().join("tmp")
    }

    /// A production-shaped home, as far as the child can tell.
    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// The default profile's locations under [`Self::home`].
    fn production_dirs(&self) -> [PathBuf; 3] {
        let home = self.home();
        [
            home.join(".local/state/phux"),
            home.join(".config/phux"),
            home.join(".local/share/phux"),
        ]
    }

    fn assert_production_untouched(&self) {
        for dir in self.production_dirs() {
            assert!(!dir.exists(), "{} was written", dir.display());
        }
    }

    /// `phux ARGS` with `HOME` as given, a scrubbed `PHUX_*` environment,
    /// no `XDG_*` bases (so every default derives from `HOME`), and a dead
    /// socket so nothing can dial a server.
    fn phux_with_home(&self, home: &Path, env: &[(&str, &Path)], args: &[&str]) -> Output {
        let mut cmd = crate::common::phux_cmd(PHUX);
        for key in [
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            cmd.env_remove(key);
        }
        cmd.env("HOME", home)
            .env("TMPDIR", self.tmp())
            .env("PHUX_SOCKET", self.root.path().join("dead.sock"))
            .env("PHUX_TAILSCALE", "phux-test-no-such-overlay-command");
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run phux")
    }

    fn phux(&self, env: &[(&str, &Path)], args: &[&str]) -> Output {
        self.phux_with_home(&self.home(), env, args)
    }
}

fn assert_refused(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must refuse; stderr={stderr}");
    assert!(
        stderr.contains("production phux state") && stderr.contains("development build"),
        "the refusal names what it protects: {stderr}"
    );
    assert!(
        stderr.contains("PHUX_WS_TOKENS") && stderr.contains("installed release"),
        "the refusal carries the remedy: {stderr}"
    );
}

/// The incident: a dev build inherits a pane's `PHUX_WS_TOKENS` naming the
/// production credential store.
#[test]
fn an_inherited_production_token_store_is_refused() {
    let sandbox = Sandbox::new();
    let store = sandbox.home().join(".local/state/phux/remote-tokens");
    let out = sandbox.phux(
        &[("PHUX_WS_TOKENS", &store)],
        &["pair", "revoke", "credential-id"],
    );
    assert_refused(&out);
    sandbox.assert_production_untouched();
}

/// `PHUX_PROFILE=default` aims every default at production; nothing runs.
#[test]
fn a_production_state_directory_refuses_every_verb() {
    let sandbox = Sandbox::new();
    let out = sandbox.phux(&[("PHUX_PROFILE", Path::new("default"))], &["host", "ls"]);
    assert_refused(&out);
    sandbox.assert_production_untouched();
}

/// The relay's state is not profile-scoped, so its guard is its own.
#[test]
fn relay_enrollment_into_production_is_refused_and_a_sandbox_is_not() {
    let sandbox = Sandbox::new();
    let out = sandbox.phux(&[], &["relay", "pair", "--route", "alpha"]);
    assert_refused(&out);
    sandbox.assert_production_untouched();

    // The layout test harnesses use: HOME inside the temp directory.
    let sandboxed_home = sandbox.tmp().join("home");
    let out = sandbox.phux_with_home(&sandboxed_home, &[], &["relay", "pair", "--route", "alpha"]);
    assert!(
        out.status.success(),
        "a temp sandbox is not production: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        sandboxed_home
            .join(".local/state/phux/relay-tokens")
            .exists()
    );
    sandbox.assert_production_untouched();
}

/// The machine registry in the production config is refused.
#[test]
fn registering_a_host_in_the_production_config_is_refused() {
    let sandbox = Sandbox::new();
    let out = sandbox.phux(&[], &["host", "add", "mini", "ssh://me@mini"]);
    assert_refused(&out);
    sandbox.assert_production_untouched();
}
