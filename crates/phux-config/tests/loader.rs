//! Integration tests for `phux_config::loader`. The path-resolution test
//! mutates process env, which is why this is its own test binary.

use std::path::PathBuf;
use std::sync::Mutex;

use phux_config::{ConfigError, loader};
use tempfile::TempDir;

/// Serializes env mutation within this binary.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Snapshots a variable and restores it on drop.
struct EnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: process-global env mutation. All env-touching tests in
        // this file acquire `ENV_LOCK` before constructing an `EnvGuard`,
        // so no other thread races us. Restored on drop.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }

    fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        // SAFETY: same as `set` — gated by `ENV_LOCK`.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: gated by `ENV_LOCK` for the lifetime of the test.
        unsafe {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A missing file is the shipped defaults; a present file layers over
/// them; any other I/O error (here `EISDIR`) propagates.
#[test]
fn load_from_layers_the_file_over_the_shipped_defaults() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = loader::load_from(&tmp.path().join("absent.toml")).expect("missing is fine");
    assert_eq!(
        cfg.keybindings.prefix_table.get("d"),
        Some(&phux_config::Action::Bare("detach".to_owned()))
    );
    assert!(!cfg.status.right.is_empty());

    let path = tmp.path().join("config.toml");
    std::fs::write(
        &path,
        "[defaults]\nshell = \"/bin/zsh\"\nhistory-limit = 1234\n",
    )
    .expect("write config");
    let cfg = loader::load_from(&path).expect("valid config parses");
    assert_eq!(cfg.defaults.shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(cfg.defaults.history_limit, 1234);
    assert_eq!(cfg.keybindings.prefix, "C-a");

    assert!(matches!(
        loader::load_from(tmp.path()),
        Err(ConfigError::Io(_))
    ));
}

/// `XDG_CONFIG_HOME` wins over `HOME`; without it, `~/.config` is used.
#[test]
fn config_path_prefers_xdg_then_home() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let _home = EnvGuard::set("HOME", "/tmp/x");
    {
        let _xdg = EnvGuard::set("XDG_CONFIG_HOME", "/tmp/whatever");
        assert_eq!(
            loader::config_path(),
            PathBuf::from("/tmp/whatever/phux/config.toml")
        );
    }
    let _xdg = EnvGuard::unset("XDG_CONFIG_HOME");
    assert_eq!(
        loader::config_path(),
        PathBuf::from("/tmp/x/.config/phux/config.toml")
    );
}
