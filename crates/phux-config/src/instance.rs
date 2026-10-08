//! Instance isolation: profile-scoped runtime and state locations.
//!
//! Every phux process — daemon, CLI verb, MCP adapter — agrees on *which*
//! phux it is talking to by resolving a **profile** here. The profile scopes
//! the Unix-domain-socket directory and the state directory, so a development
//! build can never adopt, steal, or corrupt the socket and logs of the
//! production build a user relies on day to day (phux-zomb.2).
//!
//! Resolution is deliberately automatic. Requiring a developer to remember an
//! environment variable is the same as not having isolation at all: the one
//! time it is forgotten is the time a `cargo run` unlinks the production
//! socket and takes a day's worth of panes with it.

use std::path::{Path, PathBuf};

/// The profile name reserved for the user's day-to-day installation.
///
/// Stored unsuffixed on disk (`/tmp/phux-$USER`, `$XDG_STATE_HOME/phux`) so
/// upgrading into a profile-aware build does not strand the paths a previous
/// release already created.
pub const DEFAULT_PROFILE: &str = "default";

/// The profile a build that is not a released artifact resolves to.
pub const DEV_PROFILE: &str = "dev";

/// Resolve the active profile name.
///
/// Precedence:
/// 1. `$PHUX_PROFILE` when set to a non-empty value — the explicit override
///    used by the repo's dev shell, the test harness, and anyone running more
///    than two instances on one machine;
/// 2. [`DEV_PROFILE`] when this executable is not a released artifact (see
///    [`is_dev_build`]);
/// 3. [`DEFAULT_PROFILE`].
#[must_use]
pub fn profile() -> String {
    if let Some(raw) = std::env::var_os("PHUX_PROFILE") {
        let name = raw.to_string_lossy().trim().to_owned();
        if !name.is_empty() {
            return sanitize_profile(&name);
        }
    }
    if is_dev_build() {
        return DEV_PROFILE.to_owned();
    }
    DEFAULT_PROFILE.to_owned()
}

/// Whether the active profile is the day-to-day installation.
#[must_use]
pub fn is_default_profile() -> bool {
    profile() == DEFAULT_PROFILE
}

/// Reduce a profile name to characters that are safe in a path segment.
///
/// A profile reaches a directory name, so a value carrying `/` or `..` would
/// escape the runtime root. Anything outside `[A-Za-z0-9._-]` collapses to
/// `-`, and an empty result falls back to [`DEV_PROFILE`] rather than to
/// [`DEFAULT_PROFILE`]: a caller who asked for a *named* instance and got a
/// silently unusable name must not be aliased onto production.
fn sanitize_profile(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', '-']).to_owned();
    if trimmed.is_empty() {
        DEV_PROFILE.to_owned()
    } else {
        trimmed
    }
}

/// Which kind of build this executable is, for the dev/production boundary.
///
/// Profiles keep a dev build's *default* paths apart, but an explicit
/// `--socket`, `PHUX_SOCKET`, `PHUX_PROFILE=default`, or a dev binary copied
/// over the installed one all walk straight past a default. [`BuildKind`] is
/// what the hard guards key on instead ([`crate::socket::refuse_dev_on_production`]
/// and the server's upgrade check): a [`BuildKind::Dev`] process never
/// connects to, binds, or becomes the day-to-day server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildKind {
    /// A distributed artifact: stamped `PHUX_RELEASE_ARTIFACT=1` at compile
    /// time by `scripts/build-release-binaries.sh`, the only script the
    /// release and next-release workflows build with.
    Release,
    /// An optimised build nobody stamped and that sits outside `target/`:
    /// `cargo install --path`, an app-bundled CLI, a copied `--release`.
    Local,
    /// A development build ([`is_dev_build`]).
    Dev,
}

impl BuildKind {
    /// The token `PHUX_PROBE_BUILD_KIND=1 phux` prints; see [`Self::parse`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Local => "local",
            Self::Dev => "dev",
        }
    }

    /// Parse a probe's output. `None` for a binary that predates the probe
    /// and answered with something else (its `--version` line).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "release" => Some(Self::Release),
            "local" => Some(Self::Local),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }
}

/// The environment variable that makes `phux` print its [`BuildKind`] and
/// exit, before parsing anything. A binary that predates it ignores the
/// variable and runs the accompanying `--version` instead.
pub const PROBE_BUILD_KIND_ENV: &str = "PHUX_PROBE_BUILD_KIND";

/// Classify this executable.
#[must_use]
pub fn build_kind() -> BuildKind {
    if is_dev_build() {
        return BuildKind::Dev;
    }
    if option_env!("PHUX_RELEASE_ARTIFACT").is_some_and(|stamp| stamp == "1") {
        return BuildKind::Release;
    }
    BuildKind::Local
}

/// Whether this executable is a development build rather than a released one.
///
/// Two independent signals, either of which is sufficient:
///
/// * `debug_assertions` — an unoptimised `cargo build` / `cargo test` binary;
/// * the executable sits under a Cargo `target/` directory — which catches the
///   case `debug_assertions` misses, a local `cargo build --release` or
///   `cargo run --release` inside a checkout or an agent's worktree.
///
/// A released artifact (Homebrew, a distro package, `cargo install`ed into
/// `~/.cargo/bin`) matches neither and resolves to [`DEFAULT_PROFILE`].
#[must_use]
pub fn is_dev_build() -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    std::env::current_exe().is_ok_and(|exe| exe_is_under_cargo_target(&exe))
}

/// Whether `exe` lives under a Cargo `target/` directory.
///
/// Matches `…/target/release/phux` and `…/target/<triple>/release/phux`
/// alike by looking for a `target` component with at least one component
/// after it, rather than pinning a fixed depth.
fn exe_is_under_cargo_target(exe: &Path) -> bool {
    let mut components = exe.components();
    while let Some(component) = components.next() {
        if component.as_os_str() == "target" && components.clone().next().is_some() {
            return true;
        }
    }
    false
}

/// The per-user, per-profile runtime directory holding the socket and the
/// spawn lock.
///
/// `$XDG_RUNTIME_DIR/phux[-<profile>]` when `XDG_RUNTIME_DIR` is set,
/// otherwise `/run/user/<uid>/phux[-<profile>]` when that login runtime
/// directory exists and belongs to this user, otherwise
/// `/tmp/phux-<user>[-<profile>]`. The default profile is unsuffixed so
/// paths created by earlier releases stay valid.
///
/// The `/run/user/<uid>` step matters for sessions that do not set
/// `XDG_RUNTIME_DIR` (Tailscale SSH, cron): without it they look under
/// `/tmp` while the systemd `--user` server, which always has the variable,
/// listens under `/run/user/<uid>`.
#[must_use]
pub fn runtime_dir() -> PathBuf {
    let profile = profile();
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(login_runtime_dir);
    if let Some(mut path) = base {
        path.push(suffixed("phux", &profile));
        return path;
    }
    let mut path = PathBuf::from("/tmp");
    path.push(suffixed(&format!("phux-{}", user_segment()), &profile));
    path
}

/// The per-user, per-profile state directory holding logs and provisioned
/// credentials.
///
/// `$XDG_STATE_HOME/phux[-<profile>]`, else `$HOME/.local/state/phux[-…]`.
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
    base.join(suffixed("phux", &profile()))
}

/// Directory of local bug-report bundles: `$XDG_STATE_HOME/<profile-dir>/reports`.
///
/// Written by the TUI `report-bug` action and `phux report new`; listed by
/// `phux report`. Each subdirectory is one report; `latest` is a pointer at
/// the newest.
#[must_use]
pub fn reports_dir() -> PathBuf {
    state_dir().join("reports")
}

/// Append `-<profile>` unless the profile is the default one.
fn suffixed(stem: &str, profile: &str) -> String {
    if profile == DEFAULT_PROFILE {
        stem.to_owned()
    } else {
        format!("{stem}-{profile}")
    }
}

/// The path segment identifying the user, for the `/tmp` fallback.
///
/// Only needs to be unique per user on a shared machine; the directory is
/// created `0700` and its ownership is verified before use.
/// This user's `/run/user/<uid>`, if the login manager created it.
fn login_runtime_dir() -> Option<PathBuf> {
    let uid = nix::unistd::getuid().as_raw();
    let dir = PathBuf::from(format!("/run/user/{uid}"));
    owned_dir(&dir, uid).then_some(dir)
}

fn owned_dir(dir: &Path, uid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(dir).is_ok_and(|meta| meta.is_dir() && meta.uid() == uid)
}

pub(crate) fn user_segment() -> String {
    std::env::var("UID")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_runtime_dir_must_exist_and_be_ours() {
        let dir = tempfile::tempdir().expect("tempdir");
        let uid = nix::unistd::getuid().as_raw();
        assert!(owned_dir(dir.path(), uid));
        assert!(!owned_dir(dir.path(), uid.wrapping_add(1)));
        assert!(!owned_dir(&dir.path().join("missing"), uid));
    }

    #[test]
    fn default_profile_paths_are_unsuffixed() {
        assert_eq!(suffixed("phux", DEFAULT_PROFILE), "phux");
        assert_eq!(suffixed("phux-phall", DEFAULT_PROFILE), "phux-phall");
    }

    #[test]
    fn non_default_profile_paths_carry_the_suffix() {
        assert_eq!(suffixed("phux", DEV_PROFILE), "phux-dev");
        assert_eq!(suffixed("phux-phall", "wt7"), "phux-phall-wt7");
    }

    #[test]
    fn profile_names_cannot_escape_the_runtime_root() {
        assert_eq!(sanitize_profile("../../etc"), "etc");
        assert_eq!(sanitize_profile("a/b"), "a-b");
        assert_eq!(sanitize_profile("ok-name_1.2"), "ok-name_1.2");
    }

    #[test]
    fn an_unusable_profile_name_falls_back_to_dev_not_production() {
        // A caller who asked for a named instance must never be aliased onto
        // the day-to-day socket by a sanitising accident.
        assert_eq!(sanitize_profile("///"), DEV_PROFILE);
        assert_eq!(sanitize_profile("..."), DEV_PROFILE);
    }

    #[test]
    fn build_kind_probe_round_trips_and_ignores_old_binaries() {
        for kind in [BuildKind::Release, BuildKind::Local, BuildKind::Dev] {
            assert_eq!(
                BuildKind::parse(&format!("{}\n", kind.as_str())),
                Some(kind)
            );
        }
        // A binary from before the probe prints its `--version` line.
        assert_eq!(BuildKind::parse("phux 0.45.0\n"), None);
    }

    #[test]
    fn test_binaries_are_dev_builds() {
        assert_eq!(build_kind(), BuildKind::Dev);
    }

    #[test]
    fn cargo_target_layouts_are_recognised_as_dev() {
        assert!(exe_is_under_cargo_target(Path::new(
            "/home/u/src/phux/target/release/phux"
        )));
        assert!(exe_is_under_cargo_target(Path::new(
            "/home/u/src/phux/target/aarch64-apple-darwin/release/phux"
        )));
        assert!(exe_is_under_cargo_target(Path::new(
            "/w/.claude/worktrees/x/target/debug/phux"
        )));
    }

    #[test]
    fn installed_layouts_are_not_dev() {
        assert!(!exe_is_under_cargo_target(Path::new(
            "/opt/homebrew/bin/phux"
        )));
        assert!(!exe_is_under_cargo_target(Path::new("/usr/local/bin/phux")));
        assert!(!exe_is_under_cargo_target(Path::new(
            "/home/u/.cargo/bin/phux"
        )));
        // A trailing `target` with nothing after it is a binary *named*
        // target, not a build directory.
        assert!(!exe_is_under_cargo_target(Path::new(
            "/usr/local/bin/target"
        )));
    }
}
