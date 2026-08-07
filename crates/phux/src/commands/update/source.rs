//! Where the running `phux` came from — decided from the **resolved** path of
//! the running executable, never from a guess.
//!
//! `phux update` is allowed to overwrite a binary only when it can positively
//! recognize the layout it is overwriting. That inverts the usual default: an
//! unrecognized location is a refusal, not a best-effort write. A package
//! manager owns its files (Homebrew's Cellar, Cargo's bin directory) and a Nix
//! store path is read-only by construction; mutating either one produces a
//! binary the owning tool believes is something else.
//!
//! Detection is a pure function of a [`Probe`] — the resolved executable path
//! plus the handful of environment values that move these layouts around — so
//! every arm is unit-testable without a real install.
//!
//! Order matters: the checks run most-specific first, because the layouts
//! overlap. `/usr/local/bin/phux` is the direct-release location on Linux
//! *and* the Homebrew symlink location on an Intel Mac; resolving symlinks
//! first is what separates them, since the Homebrew entry resolves into
//! `…/Cellar/phux/<version>/bin/phux` and the direct-release one does not.

use std::path::{Component, Path, PathBuf};

/// The default Nix store prefix, overridable through `NIX_STORE`.
const DEFAULT_NIX_STORE: &str = "/nix/store";

/// The marker file every `NixOS` system carries; its absence means a Nix
/// install on a non-NixOS host (a profile or home-manager install), which has
/// a different update command.
const NIXOS_MARKER: &str = "/etc/NIXOS";

/// Linuxbrew's default prefix. Homebrew on macOS lives at `/opt/homebrew`
/// (arm64) or `/usr/local` (`x86_64`); both are recognized by their `Cellar`
/// component rather than by prefix, which is what makes a relocated
/// `HOMEBREW_PREFIX` work too.
const LINUXBREW_PREFIX: &str = "/home/linuxbrew/.linuxbrew";

/// How `phux` was installed.
///
/// The variants are exactly the five cases the update path has to tell apart;
/// [`InstallSource::is_mutable`] is the single question the rest of the
/// command asks of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallSource {
    /// A GitHub release tarball, unpacked into a recognized bin directory by
    /// the curl installer or by hand. The one layout `phux update` writes to.
    DirectRelease,
    /// Homebrew (or Linuxbrew): the resolved path lives inside a Cellar.
    Homebrew,
    /// `cargo install` into `$CARGO_HOME/bin`.
    Cargo,
    /// A `/nix/store` path — read-only by construction.
    Nix,
    /// Anything else. Refused rather than overwritten.
    Unknown,
}

impl InstallSource {
    /// The stable string this source is called in `--json` output.
    ///
    /// Part of the frozen `--json` surface (ADR-0071): renaming one of these
    /// is a breaking change.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::DirectRelease => "direct-release",
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::Nix => "nix",
            Self::Unknown => "unknown",
        }
    }

    /// Whether `phux update` may replace files at this install's path.
    ///
    /// Only the direct-release layout says yes. Everything else either has an
    /// owner (Homebrew, Cargo) or is physically read-only (Nix).
    pub(crate) const fn is_mutable(self) -> bool {
        matches!(self, Self::DirectRelease)
    }
}

/// Why an install was classified [`InstallSource::Unknown`], so the refusal
/// can say something more useful than "unrecognized".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownReason {
    /// The path looks like a cargo build directory (`…/target/{debug,release}`),
    /// i.e. a checkout the user builds themselves.
    BuildDirectory,
    /// No positive evidence for any recognized layout.
    Unrecognized,
}

/// One resolved install: the source, the executable it was decided from, and
/// the reason when that source is [`InstallSource::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Install {
    /// The classification.
    pub(crate) source: InstallSource,
    /// The symlink-resolved path of the running executable.
    pub(crate) executable: PathBuf,
    /// Set when `source` is [`InstallSource::Nix`] and the host is `NixOS`,
    /// which changes the update command from `nix profile upgrade` to a
    /// system rebuild.
    pub(crate) nixos: bool,
    /// Set when `source` is [`InstallSource::Unknown`].
    pub(crate) unknown_reason: Option<UnknownReason>,
}

impl Install {
    /// The directory the binaries live in, when there is one.
    pub(crate) fn bin_dir(&self) -> Option<&Path> {
        self.executable.parent()
    }

    /// The exact native command that updates this install, as a block of
    /// lines, or `None` when `phux update` handles it itself.
    ///
    /// These strings are the whole product of the package-managed and
    /// immutable arms: a user who is told "phux cannot update this" and not
    /// told what can has been given a dead end.
    pub(crate) fn native_command(&self) -> Option<String> {
        match self.source {
            InstallSource::DirectRelease | InstallSource::Unknown => None,
            InstallSource::Homebrew => Some("brew upgrade phall1/tap/phux".to_owned()),
            InstallSource::Cargo => Some(
                "from a phux checkout:\n  \
                 nix develop -c cargo install --locked --path crates/phux\n  \
                 nix develop -c cargo install --locked --path crates/phux-mcp"
                    .to_owned(),
            ),
            InstallSource::Nix => Some(if self.nixos {
                "update the flake input that provides phux, then rebuild:\n  \
                 nix flake update phux\n  \
                 sudo nixos-rebuild switch"
                    .to_owned()
            } else {
                "nix profile upgrade phux\n\
                 (home-manager installs: update the input, then `home-manager switch`)"
                    .to_owned()
            }),
        }
    }
}

/// Everything [`detect`] reads, gathered up front so the decision itself is
/// pure and every arm is reachable from a test.
#[derive(Debug, Clone, Default)]
pub(crate) struct Probe {
    /// The running executable, symlinks already resolved.
    pub(crate) executable: PathBuf,
    /// `$HOME`.
    pub(crate) home: Option<PathBuf>,
    /// `$CARGO_HOME`, if set; otherwise `$HOME/.cargo` is assumed.
    pub(crate) cargo_home: Option<PathBuf>,
    /// `$PHUX_INSTALL_DIR` — the curl installer's override, which is the only
    /// way a direct-release install lands somewhere unusual on purpose.
    pub(crate) install_dir: Option<PathBuf>,
    /// `$NIX_STORE`, if set; otherwise `/nix/store`.
    pub(crate) nix_store: Option<PathBuf>,
    /// `$HOMEBREW_PREFIX`, if set.
    pub(crate) homebrew_prefix: Option<PathBuf>,
    /// Whether `/etc/NIXOS` exists.
    pub(crate) nixos: bool,
}

impl Probe {
    /// Read a [`Probe`] from the live process: the resolved `current_exe`
    /// plus the environment.
    ///
    /// The symlink resolution is the load-bearing step. `~/.local/bin/phux`
    /// may be a symlink into a Cellar, a Nix store path, or a checkout's
    /// `target/release`; classifying the link instead of its destination
    /// would let `phux update` overwrite a symlink and silently orphan the
    /// real binary.
    pub(crate) fn from_process() -> std::io::Result<Self> {
        let raw = std::env::current_exe()?;
        // `canonicalize` resolves every symlink in the path, including
        // intermediate directory links (`/var` -> `/private/var` on macOS).
        // If it fails the raw path is still better than nothing — but a path
        // we could not resolve is exactly the case that must not be
        // overwritten, so an unresolvable path stays unresolved and will fall
        // through to `Unknown` unless it matches on its own.
        let executable = std::fs::canonicalize(&raw).unwrap_or(raw);
        Ok(Self {
            executable,
            home: std::env::var_os("HOME").map(PathBuf::from),
            cargo_home: std::env::var_os("CARGO_HOME").map(PathBuf::from),
            install_dir: std::env::var_os("PHUX_INSTALL_DIR").map(PathBuf::from),
            nix_store: std::env::var_os("NIX_STORE").map(PathBuf::from),
            homebrew_prefix: std::env::var_os("HOMEBREW_PREFIX").map(PathBuf::from),
            nixos: Path::new(NIXOS_MARKER).exists(),
        })
    }
}

/// Classify `probe`'s executable. Pure; see the module docs for the ordering
/// rationale.
pub(crate) fn detect(probe: &Probe) -> Install {
    let exe = probe.executable.clone();
    let source = classify(probe);
    let unknown_reason = if source == InstallSource::Unknown {
        Some(if looks_like_build_directory(&exe) {
            UnknownReason::BuildDirectory
        } else {
            UnknownReason::Unrecognized
        })
    } else {
        None
    };
    Install {
        source,
        executable: exe,
        nixos: probe.nixos,
        unknown_reason,
    }
}

/// The classification ladder, most specific first.
fn classify(probe: &Probe) -> InstallSource {
    let exe = probe.executable.as_path();

    // 1. Nix store. Read-only by construction, so it is checked before any
    //    prefix that a store path could also sit under.
    let store = probe
        .nix_store
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_NIX_STORE));
    if exe.starts_with(&store) {
        return InstallSource::Nix;
    }

    // 2. Homebrew. The Cellar component is the reliable marker across
    //    `/opt/homebrew`, `/usr/local`, Linuxbrew, and relocated prefixes;
    //    the prefix checks catch a keg-only or otherwise unusual layout.
    if has_component(exe, "Cellar")
        || probe
            .homebrew_prefix
            .as_ref()
            .is_some_and(|prefix| exe.starts_with(prefix))
        || exe.starts_with(LINUXBREW_PREFIX)
    {
        return InstallSource::Homebrew;
    }

    // 3. Cargo's bin directory.
    let cargo_bin = probe
        .cargo_home
        .clone()
        .or_else(|| probe.home.as_ref().map(|home| home.join(".cargo")))
        .map(|root| root.join("bin"));
    if is_child_of(exe, cargo_bin.as_deref()) {
        return InstallSource::Cargo;
    }

    // 4. Direct release. The allowlist is `$PHUX_INSTALL_DIR` (the curl
    //    installer's own override) plus the locations that installer and
    //    docs/INSTALL.md actually name. Anything else is not assumed.
    if direct_release_dirs(probe)
        .iter()
        .any(|dir| is_child_of(exe, Some(dir)))
    {
        return InstallSource::DirectRelease;
    }

    InstallSource::Unknown
}

/// The recognized direct-release bin directories, in precedence order.
fn direct_release_dirs(probe: &Probe) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = probe.install_dir.clone() {
        dirs.push(dir);
    }
    if let Some(home) = probe.home.as_ref() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join("bin"));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs.push(PathBuf::from("/opt/phux/bin"));
    dirs
}

/// Whether `path`'s immediate parent is `dir`.
fn is_child_of(path: &Path, dir: Option<&Path>) -> bool {
    match (path.parent(), dir) {
        (Some(parent), Some(dir)) => parent == dir,
        _ => false,
    }
}

/// Whether any normal component of `path` is exactly `name`.
fn has_component(path: &Path, name: &str) -> bool {
    path.components().any(|component| match component {
        Component::Normal(part) => part == name,
        _ => false,
    })
}

/// Whether `path` looks like it came out of `cargo build` in a checkout —
/// `…/target/debug/phux` or `…/target/release/phux`, including the
/// `--target <triple>` layout.
fn looks_like_build_directory(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let profile = parent.file_name().and_then(|name| name.to_str());
    if !matches!(profile, Some("debug" | "release")) {
        return false;
    }
    has_component(path, "target")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{InstallSource, Probe, UnknownReason, detect};

    /// A probe for a plausible Linux/macOS user with nothing unusual set.
    fn probe(exe: &str) -> Probe {
        Probe {
            executable: PathBuf::from(exe),
            home: Some(PathBuf::from("/home/ada")),
            ..Probe::default()
        }
    }

    #[test]
    fn direct_release_is_recognized_in_every_documented_bin_dir() {
        for path in [
            "/home/ada/.local/bin/phux",
            "/home/ada/bin/phux",
            "/usr/local/bin/phux",
            "/opt/phux/bin/phux",
        ] {
            let install = detect(&probe(path));
            assert_eq!(
                install.source,
                InstallSource::DirectRelease,
                "{path} should be a direct-release install"
            );
            assert!(install.source.is_mutable());
            assert!(install.native_command().is_none());
        }
    }

    #[test]
    fn phux_install_dir_extends_the_direct_release_allowlist() {
        let mut probe = probe("/srv/tools/phux");
        assert_eq!(detect(&probe).source, InstallSource::Unknown);
        probe.install_dir = Some(PathBuf::from("/srv/tools"));
        assert_eq!(detect(&probe).source, InstallSource::DirectRelease);
    }

    #[test]
    fn homebrew_is_recognized_by_its_cellar_on_every_prefix() {
        for path in [
            "/opt/homebrew/Cellar/phux/0.12.1/bin/phux",
            "/usr/local/Cellar/phux/0.12.1/bin/phux",
            "/home/linuxbrew/.linuxbrew/Cellar/phux/0.12.1/bin/phux",
        ] {
            let install = detect(&probe(path));
            assert_eq!(install.source, InstallSource::Homebrew, "{path}");
            assert!(!install.source.is_mutable());
            assert_eq!(
                install.native_command().as_deref(),
                Some("brew upgrade phall1/tap/phux")
            );
        }
    }

    #[test]
    fn a_relocated_homebrew_prefix_is_still_homebrew() {
        let mut probe = probe("/data/brew/bin/phux");
        probe.homebrew_prefix = Some(PathBuf::from("/data/brew"));
        assert_eq!(detect(&probe).source, InstallSource::Homebrew);
    }

    #[test]
    fn cargo_bin_is_recognized_through_home_and_cargo_home() {
        let install = detect(&probe("/home/ada/.cargo/bin/phux"));
        assert_eq!(install.source, InstallSource::Cargo);
        assert!(!install.source.is_mutable());
        let native = install.native_command().unwrap_or_default();
        assert!(native.contains("cargo install --locked --path crates/phux"));

        let mut relocated = probe("/opt/cargo/bin/phux");
        relocated.cargo_home = Some(PathBuf::from("/opt/cargo"));
        assert_eq!(detect(&relocated).source, InstallSource::Cargo);
    }

    #[test]
    fn nix_store_paths_are_never_mutable_and_name_the_right_rebuild() {
        let mut probe = probe("/nix/store/abc123-phux-0.12.1/bin/phux");
        let install = detect(&probe);
        assert_eq!(install.source, InstallSource::Nix);
        assert!(!install.source.is_mutable());
        let profile = install.native_command().unwrap_or_default();
        assert!(profile.contains("nix profile upgrade phux"), "{profile}");

        probe.nixos = true;
        let system = detect(&probe).native_command().unwrap_or_default();
        assert!(system.contains("nixos-rebuild switch"), "{system}");
        assert!(system.contains("nix flake update"), "{system}");
    }

    #[test]
    fn a_relocated_nix_store_is_still_nix() {
        let mut probe = probe("/data/nix/store/abc-phux/bin/phux");
        probe.nix_store = Some(PathBuf::from("/data/nix/store"));
        assert_eq!(detect(&probe).source, InstallSource::Nix);
    }

    #[test]
    fn unknown_locations_fail_safe_rather_than_defaulting_to_direct_release() {
        let install = detect(&probe("/usr/bin/phux"));
        assert_eq!(install.source, InstallSource::Unknown);
        assert!(!install.source.is_mutable());
        assert_eq!(install.unknown_reason, Some(UnknownReason::Unrecognized));
        assert!(install.native_command().is_none());
    }

    #[test]
    fn a_checkout_build_directory_says_so() {
        for path in [
            "/home/ada/src/phux/target/debug/phux",
            "/home/ada/src/phux/target/aarch64-apple-darwin/release/phux",
        ] {
            let install = detect(&probe(path));
            assert_eq!(install.source, InstallSource::Unknown, "{path}");
            assert_eq!(
                install.unknown_reason,
                Some(UnknownReason::BuildDirectory),
                "{path}"
            );
        }
    }

    /// A Nix store path under a Homebrew-looking prefix must still classify
    /// as Nix: the ladder checks the read-only store first.
    #[test]
    fn nix_wins_over_the_other_arms() {
        let mut probe = probe("/nix/store/abc-phux/Cellar/phux/bin/phux");
        probe.homebrew_prefix = Some(PathBuf::from("/nix"));
        assert_eq!(detect(&probe).source, InstallSource::Nix);
    }

    /// Every source renders a distinct, stable `--json` token.
    #[test]
    fn source_tokens_are_the_frozen_vocabulary() {
        assert_eq!(InstallSource::DirectRelease.as_str(), "direct-release");
        assert_eq!(InstallSource::Homebrew.as_str(), "homebrew");
        assert_eq!(InstallSource::Cargo.as_str(), "cargo");
        assert_eq!(InstallSource::Nix.as_str(), "nix");
        assert_eq!(InstallSource::Unknown.as_str(), "unknown");
    }
}
