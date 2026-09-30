//! Production state: the files a development build must never touch.
//!
//! Profiles keep a dev build's *default* state apart (`phux-dev` next to
//! `phux`), but a path handed over explicitly walks straight past a default:
//! a pane of the production server exports `PHUX_WS_TOKENS`,
//! `PHUX_WS_TLS_CERT`, and `PHUX_WS_TLS_KEY`, so a test or agent run from
//! that pane inherits the operator's credential store. [`refuse_dev_on_production_state`]
//! is the one check every write of phux state (and every read of a
//! production secret) goes through, mirroring the socket guard
//! ([`crate::socket::refuse_dev_on_production`]): no override.
//!
//! "Production" is the default profile's real locations, not "the default
//! profile in this environment". Test harnesses pin the released layout
//! (`PHUX_PROFILE=default`) inside a temp sandbox, and that sandbox is not
//! production. So the roots are derived from the account's home directory
//! as the password database records it (never exempt, whatever `$HOME`
//! says), plus `$HOME` and the `XDG_*` bases unless they sit inside the temp
//! directory.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::instance;

/// The launchd label and systemd unit of the default profile's service.
/// `phux service` derives its unit paths from the same names.
pub const PRODUCTION_LAUNCHD_PLIST: &str = "com.phux.server.plist";
/// See [`PRODUCTION_LAUNCHD_PLIST`].
pub const PRODUCTION_SYSTEMD_UNIT: &str = "phux.service";

/// Where the day-to-day installation's state could live, before resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Layout {
    /// Home directories whose default locations are production.
    homes: Vec<PathBuf>,
    /// `$XDG_STATE_HOME`, when it is production.
    xdg_state: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`, when it is production.
    xdg_config: Option<PathBuf>,
    /// `$XDG_DATA_HOME`, when it is production.
    xdg_data: Option<PathBuf>,
}

impl Layout {
    /// A layout from the account home and an environment lookup: `$HOME`
    /// and each `XDG_*` base count unless they sit inside `temp`.
    fn from_vars(
        account_home: Option<PathBuf>,
        temp: &Path,
        env: impl Fn(&str) -> Option<OsString>,
    ) -> Self {
        let production = |var: &str| {
            env(var)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute() && !is_inside(dir, temp))
        };
        let mut homes: Vec<PathBuf> = account_home.into_iter().collect();
        if let Some(home) = production("HOME")
            && !homes.iter().any(|known| same_path(known, &home))
        {
            homes.push(home);
        }
        Self {
            homes,
            xdg_state: production("XDG_STATE_HOME"),
            xdg_config: production("XDG_CONFIG_HOME"),
            xdg_data: production("XDG_DATA_HOME"),
        }
    }

    /// Every production location: the unsuffixed state, config, and data
    /// directories, and the default profile's service unit.
    fn roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for home in &self.homes {
            roots.push(home.join(".local/state/phux"));
            roots.push(home.join(".config/phux"));
            roots.push(home.join(".local/share/phux"));
            roots.push(
                home.join(".config/systemd/user")
                    .join(PRODUCTION_SYSTEMD_UNIT),
            );
            roots.push(
                home.join("Library/LaunchAgents")
                    .join(PRODUCTION_LAUNCHD_PLIST),
            );
        }
        if let Some(state) = &self.xdg_state {
            roots.push(state.join("phux"));
        }
        if let Some(config) = &self.xdg_config {
            roots.push(config.join("phux"));
            roots.push(config.join("systemd/user").join(PRODUCTION_SYSTEMD_UNIT));
        }
        if let Some(data) = &self.xdg_data {
            roots.push(data.join("phux"));
        }
        roots
    }

    /// The production root `path` falls inside, if any.
    fn root_containing(&self, path: &Path) -> Option<PathBuf> {
        self.roots().into_iter().find(|root| is_inside(path, root))
    }
}

/// The account's home directory from the password database, independent of
/// `$HOME` (which a sandbox rewrites while inherited paths still point home).
fn account_home() -> Option<PathBuf> {
    nix::unistd::User::from_uid(nix::unistd::getuid())
        .ok()
        .flatten()
        .map(|user| user.dir)
        .filter(|dir| dir.is_absolute())
}

fn current_layout() -> Layout {
    Layout::from_vars(account_home(), &std::env::temp_dir(), |var| {
        std::env::var_os(var)
    })
}

/// The production locations on this machine, for diagnostics and docs.
#[must_use]
pub fn production_roots() -> Vec<PathBuf> {
    current_layout().roots()
}

/// Whether `path` is (inside) the day-to-day installation's state, config,
/// data, or service unit. Classification only; see
/// [`refuse_dev_on_production_state`] for the guard.
#[must_use]
pub fn is_production_state(path: &Path) -> bool {
    current_layout().root_containing(path).is_some()
}

/// Refuse to let a development build write production phux state, or read
/// a production secret.
///
/// Every writer of phux state resolves its path first and passes it here:
/// the credential store, the TLS pair, workload authority material, relay
/// enrollment, the machine registries, service units, the upload and plugin
/// directories, and (at startup) the state directory itself. A
/// [`instance::BuildKind::Dev`] process aimed at a production location, by
/// an inherited `PHUX_*` path, `PHUX_PROFILE=default`, or a symlink, gets an
/// error naming the remedy. There is deliberately no override.
///
/// # Errors
///
/// The refusal, as a message for the caller to surface.
pub fn refuse_dev_on_production_state(path: &Path) -> Result<(), String> {
    if instance::build_kind() != instance::BuildKind::Dev {
        return Ok(());
    }
    refuse_in(&current_layout(), path)
}

fn refuse_in(layout: &Layout, path: &Path) -> Result<(), String> {
    layout
        .root_containing(path)
        .map_or(Ok(()), |root| Err(refusal(path, &root)))
}

fn refusal(path: &Path, root: &Path) -> String {
    format!(
        "refusing to touch production phux state {} (inside {}) from a development \
         build; dev builds keep their own state under the `{}` profile. An inherited \
         path is the usual cause: a pane of the production server exports \
         PHUX_WS_TOKENS, PHUX_WS_TLS_CERT, and PHUX_WS_TLS_KEY, and \
         PHUX_PROFILE=default aims every default at production. Unset them, or point \
         HOME and XDG_STATE_HOME/XDG_CONFIG_HOME/XDG_DATA_HOME at a scratch directory; \
         change production state only with the installed release `phux`",
        path.display(),
        root.display(),
        instance::DEV_PROFILE,
    )
}

/// Whether `path` is `dir` or inside it, lexically or after resolving
/// symlinks (`/tmp` is `/private/tmp` on macOS). Neither needs to exist.
fn is_inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir) || resolve(path).starts_with(resolve(dir))
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || resolve(a) == resolve(b)
}

/// Make `path` absolute and canonicalise its deepest existing ancestor, so a
/// file that does not exist yet still compares through a symlinked
/// directory. `..` after a missing component is kept lexically.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    };
    let mut missing: Vec<OsString> = Vec::new();
    let mut existing = absolute.as_path();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(existing) {
            return missing
                .iter()
                .rev()
                .fold(canonical, |acc, part| acc.join(part));
        }
        let (Some(parent), Some(Component::Normal(name))) =
            (existing.parent(), existing.components().next_back())
        else {
            return absolute;
        };
        missing.push(name.to_os_string());
        existing = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(home: &Path) -> Layout {
        Layout {
            homes: vec![home.to_path_buf()],
            ..Layout::default()
        }
    }

    #[test]
    fn default_profile_locations_under_a_production_home_are_refused() {
        let home = Path::new("/phux-test-home/u");
        let layout = layout(home);
        for path in [
            "/phux-test-home/u/.local/state/phux/remote-tokens",
            "/phux-test-home/u/.local/state/phux/remote-key.pem",
            "/phux-test-home/u/.local/state/phux/workload-ca.key",
            "/phux-test-home/u/.local/state/phux",
            "/phux-test-home/u/.config/phux/config.toml",
            "/phux-test-home/u/.local/share/phux/uploads/x",
            "/phux-test-home/u/Library/LaunchAgents/com.phux.server.plist",
            "/phux-test-home/u/.config/systemd/user/phux.service",
        ] {
            let refusal = refuse_in(&layout, Path::new(path)).expect_err(path);
            assert!(refusal.contains("development build"), "{refusal}");
            assert!(refusal.contains("PHUX_WS_TOKENS"), "{refusal}");
        }
    }

    #[test]
    fn dev_profile_and_unrelated_locations_are_allowed() {
        let layout = layout(Path::new("/phux-test-home/u"));
        for path in [
            "/phux-test-home/u/.local/state/phux-dev/remote-tokens",
            "/phux-test-home/u/.local/state/phuxy",
            "/phux-test-home/u/.config/phux-dev/config.toml",
            "/phux-test-home/u/Library/LaunchAgents/com.phux.server.dev.plist",
            "/phux-test-home/u/.config/systemd/user/phux-dev.service",
            "/phux-test-home/u/scratch/remote-tokens",
            "/phux-test-home/other/.local/state/phux/remote-tokens",
        ] {
            assert!(refuse_in(&layout, Path::new(path)).is_ok(), "{path}");
        }
    }

    #[test]
    fn xdg_bases_extend_the_production_roots() {
        let layout = Layout {
            homes: Vec::new(),
            xdg_state: Some(PathBuf::from("/srv/state")),
            xdg_config: Some(PathBuf::from("/srv/config")),
            xdg_data: Some(PathBuf::from("/srv/data")),
        };
        assert!(refuse_in(&layout, Path::new("/srv/state/phux/remote-tokens")).is_err());
        assert!(refuse_in(&layout, Path::new("/srv/config/phux/config.toml")).is_err());
        assert!(refuse_in(&layout, Path::new("/srv/config/systemd/user/phux.service")).is_err());
        assert!(refuse_in(&layout, Path::new("/srv/data/phux/plugins")).is_err());
        assert!(refuse_in(&layout, Path::new("/srv/state/phux-dev/remote-tokens")).is_ok());
    }

    #[test]
    fn a_home_or_xdg_base_inside_the_temp_directory_is_a_sandbox() {
        // The sandbox test harnesses build: HOME and XDG pinned inside the
        // temp directory, with the released layout (`PHUX_PROFILE=default`).
        let temp = tempfile::tempdir().unwrap();
        let sandbox = temp.path().to_path_buf();
        let vars = |var: &str| match var {
            "HOME" => Some(sandbox.join("home").into_os_string()),
            "XDG_STATE_HOME" => Some(sandbox.join("state").into_os_string()),
            "XDG_CONFIG_HOME" => Some(sandbox.join("config").into_os_string()),
            _ => None,
        };
        let layout = Layout::from_vars(None, temp.path(), vars);
        assert_eq!(layout, Layout::default());
        for path in [
            "home/.local/state/phux/remote-tokens",
            "state/phux/remote-tokens",
            "config/phux/config.toml",
        ] {
            assert!(refuse_in(&layout, &sandbox.join(path)).is_ok(), "{path}");
        }
    }

    #[test]
    fn home_and_xdg_bases_outside_the_temp_directory_are_production() {
        // A sandbox the guard treats as production: its temp directory is
        // somewhere else (the CLI tests move `TMPDIR` to get this).
        let temp = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let vars = |var: &str| match var {
            "HOME" => Some(home.clone().into_os_string()),
            "XDG_STATE_HOME" => Some(state.clone().into_os_string()),
            "XDG_CONFIG_HOME" => Some(OsString::from("relative/config")),
            _ => None,
        };
        let layout = Layout::from_vars(None, elsewhere.path(), vars);
        assert_eq!(layout.homes, vec![home.clone()]);
        assert_eq!(layout.xdg_state, Some(state.clone()));
        assert_eq!(layout.xdg_config, None, "a relative base is ignored");
        assert!(refuse_in(&layout, &home.join(".local/state/phux/remote-tokens")).is_err());
        assert!(refuse_in(&layout, &state.join("phux/remote-cert.pem")).is_err());
    }

    #[test]
    fn the_account_home_is_production_even_when_home_is_a_sandbox() {
        // The incident shape: HOME points at a sandbox, an inherited
        // PHUX_WS_TOKENS still points at the real store.
        let account = Path::new("/Users/operator");
        let temp = tempfile::tempdir().unwrap();
        let sandbox_home = temp.path().join("home").into_os_string();
        let layout = Layout::from_vars(Some(account.to_path_buf()), temp.path(), |var| {
            (var == "HOME").then(|| sandbox_home.clone())
        });
        assert_eq!(layout.homes, vec![account.to_path_buf()]);
        assert!(
            refuse_in(
                &layout,
                Path::new("/Users/operator/.local/state/phux/remote-tokens")
            )
            .is_err()
        );
    }

    #[test]
    fn a_symlinked_directory_does_not_hide_production() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".local/state/phux")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(home.join(".local/state"), &link).unwrap();
        let layout = layout(&home);

        // Through a symlinked parent, to a file that does not exist yet.
        assert!(refuse_in(&layout, &link.join("phux/remote-tokens")).is_err());
        // A symlink whose target is the production directory itself.
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(home.join(".local/state/phux"), &alias).unwrap();
        assert!(refuse_in(&layout, &alias.join("remote-key.pem")).is_err());
        // The dev profile beside it stays allowed.
        assert!(refuse_in(&layout, &link.join("phux-dev/remote-tokens")).is_ok());
    }

    #[test]
    fn resolve_keeps_missing_components_after_the_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(resolve(&dir.path().join("a/b/c")), canonical.join("a/b/c"));
    }

    #[test]
    fn test_binaries_are_refused_the_account_home() {
        // Tests are dev builds, so the live guard applies to them too.
        let Some(home) = account_home() else {
            return;
        };
        assert!(
            refuse_dev_on_production_state(&home.join(".local/state/phux/remote-tokens")).is_err()
        );
        assert!(
            refuse_dev_on_production_state(&home.join(".local/state/phux-dev/remote-tokens"))
                .is_ok()
        );
    }
}
