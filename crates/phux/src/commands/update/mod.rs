//! `phux update` — the one-command path from the release phux is running to
//! the release phux publishes.
//!
//! ## Why this is release-candidate scope and not a convenience
//!
//! [ADR-0071](../../../../../ADR/0071-what-phux-1-0-commits-to.md) freezes the
//! consumer surface at 1.0 and deliberately does **not** freeze the wire,
//! which keeps its own `0.x` line under ADR-0061 — where a minor bump is a
//! fleet-wide break with no grace window and mismatched peers refuse each
//! other at HELLO. The compatibility unit is therefore the *release*, not the
//! frame: a deployment's server, local clients, satellites, and relays must
//! all run the same one. A fleet that cannot be moved between releases in one
//! step is a fleet that will sit on a mismatch, so "upgrade everything" has to
//! be a command rather than a runbook.
//!
//! ## The trust boundary
//!
//! * The **checksum is the trust anchor**. The `.sha256` sidecar published
//!   beside the tarball is compared against a digest computed locally, over
//!   the downloaded file, *before* anything is unpacked. A mismatch refuses
//!   loudly and installs nothing.
//! * **Nothing downloaded is executed to decide whether to install it.** The
//!   archive is treated as data throughout: member list validated, unpacked,
//!   extracted tree validated. The only place a freshly installed binary is
//!   run is the server's own pre-commit `--version` check inside the graceful
//!   upgrade, which happens after installation and where a failure is
//!   harmless — the old image keeps serving.
//! * **Replacement is atomic and permission-preserving** — see
//!   [`apply`] for the staging/rename discipline.
//! * **An install phux does not own is never mutated.** Homebrew, Cargo, and
//!   Nix each get the exact native command instead. An install phux cannot
//!   even recognize is refused outright rather than overwritten on a guess.
//!
//! ## Relationship to `phux upgrade`
//!
//! `phux upgrade` stays exactly what it was: the low-level primitive that
//! asks a running server to re-exec whatever binary is already on disk. It
//! discovers nothing and downloads nothing. `phux update` is the user-facing
//! verb built on top — it puts a new binary on disk and then calls that
//! primitive so live panes survive the swap.

pub(crate) mod apply;
pub(crate) mod release;
pub(crate) mod source;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use self::release::{Artifact, ReleaseSource, Version};
use self::source::{Install, InstallSource, UnknownReason};
use super::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE};

/// Version of the `phux update --json` document. Additive fields do not bump
/// it (ADR-0071 freezes the shape at 1.0).
const DOCUMENT_SCHEMA_VERSION: u8 = 1;

/// Everything that can go wrong between "there is a newer release" and "it is
/// installed", with the failure kept separate from how it is reported.
#[derive(Debug)]
pub(crate) enum UpdateError {
    /// A release tag that is not `vMAJOR.MINOR.PATCH`.
    InvalidTag(String),
    /// This OS/architecture has no published release artifact.
    UnsupportedPlatform(String),
    /// The release index or an artifact could not be downloaded.
    Fetch(String),
    /// The `.sha256` sidecar was unusable, or the download could not be
    /// hashed. Distinct from a mismatch: nothing disagreed, something was
    /// unreadable.
    Checksum(String),
    /// The published digest and the downloaded bytes disagree. The one
    /// failure this whole module exists to produce.
    ChecksumMismatch {
        /// The digest the release published.
        expected: String,
        /// The digest of the bytes that arrived.
        actual: String,
        /// The artifact both refer to.
        archive: String,
    },
    /// The archive's contents are not what a phux release tarball contains.
    Archive(String),
    /// Staging or replacement failed.
    Install(String),
    /// A rollback was asked for with nothing saved to roll back to.
    NoBackup(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTag(message)
            | Self::UnsupportedPlatform(message)
            | Self::Fetch(message)
            | Self::Checksum(message)
            | Self::Archive(message)
            | Self::Install(message)
            | Self::NoBackup(message) => f.write_str(message),
            Self::ChecksumMismatch {
                expected,
                actual,
                archive,
            } => write!(
                f,
                "checksum mismatch for {archive}: the release publishes \
                 {expected}, the download hashed to {actual}"
            ),
        }
    }
}

impl UpdateError {
    /// The stable `--json` error code.
    const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTag(_) => codes::UPDATE_INVALID_TAG,
            Self::UnsupportedPlatform(_) => codes::UPDATE_UNSUPPORTED_PLATFORM,
            Self::Fetch(_) => codes::UPDATE_FETCH_FAILED,
            Self::Checksum(_) => codes::UPDATE_CHECKSUM_INVALID,
            Self::ChecksumMismatch { .. } => codes::UPDATE_CHECKSUM_MISMATCH,
            Self::Archive(_) => codes::UPDATE_ARCHIVE_REJECTED,
            Self::Install(_) => codes::UPDATE_INSTALL_FAILED,
            Self::NoBackup(_) => codes::UPDATE_NO_BACKUP,
        }
    }

    /// Usage-class failures exit 2; everything else exits 1.
    const fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidTag(_) | Self::UnsupportedPlatform(_) => EXIT_USAGE,
            _ => EXIT_FAILURE,
        }
    }

    /// The way out, in the caller's own terms.
    fn remedy(&self) -> String {
        match self {
            Self::InvalidTag(_) => {
                "pass a tag from https://github.com/phall1/phux/releases, like `--version v1.2.3`"
                    .to_owned()
            }
            Self::UnsupportedPlatform(_) => {
                "build from source: see docs/INSTALL.md#from-source".to_owned()
            }
            Self::Fetch(_) => "check network access to github.com, then retry; \
                 `phux update --check` re-reads the release index"
                .to_owned(),
            Self::Checksum(_) | Self::ChecksumMismatch { .. } => {
                "nothing was installed. Re-run to download again; if it \
                 mismatches a second time, do not install this artifact by \
                 hand — report it at https://github.com/phall1/phux/issues"
                    .to_owned()
            }
            Self::Archive(_) => "nothing was installed. Re-run to download again; a repeat \
                 failure means the published artifact is malformed"
                .to_owned(),
            Self::Install(_) => "the previous binaries are saved; `phux update --rollback` \
                 restores them"
                .to_owned(),
            Self::NoBackup(_) => "a backup exists only after a successful `phux update`; \
                 reinstall with the curl installer in docs/INSTALL.md"
                .to_owned(),
        }
    }

    /// Report on the right channel and return the process status.
    fn report(&self, json: bool) -> ExitCode {
        let err = CliError::new(self.code(), self.to_string(), self.remedy());
        json_err::emit(json, &err, self.exit_code())
    }
}

/// `phux update`'s flags.
#[derive(Debug, Args)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a clap flag struct; each bool is one independent CLI switch, and \
              collapsing them into an enum would change the frozen grammar"
)]
pub(crate) struct UpdateOpts {
    /// Report the current and latest release and the install source, then
    /// stop. Changes nothing and never downloads an archive.
    #[arg(long, conflicts_with_all = ["dry_run", "rollback"])]
    pub(crate) check: bool,

    /// Do everything except the replacement: resolve, download, and verify
    /// the checksum, then report what would have been installed.
    #[arg(long, conflicts_with = "rollback")]
    pub(crate) dry_run: bool,

    /// Install this release tag instead of the latest one. Accepts any tag
    /// from the releases page, including an older one (a downgrade).
    #[arg(long = "version", value_name = "TAG", conflicts_with = "rollback")]
    pub(crate) tag: Option<String>,

    /// Restore the binaries saved by the previous `phux update`.
    #[arg(long)]
    pub(crate) rollback: bool,

    /// Replace the binaries but do not ask a running server to re-exec.
    /// Live panes keep the old image until the server is upgraded or
    /// restarted.
    #[arg(long)]
    pub(crate) no_restart: bool,

    /// Emit the stable, versioned JSON document on stdout instead of the
    /// human view. On failure, stdout stays empty and stderr carries one
    /// JSON error object.
    #[arg(long)]
    pub(crate) json: bool,
}

/// What `phux update` decided before it did anything.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// Where this binary came from.
    pub(crate) install: Install,
    /// The version this binary was built as.
    pub(crate) current: Version,
    /// The newest published release.
    pub(crate) latest_tag: String,
    /// The release that would be installed — `latest_tag` unless `--version`
    /// named another.
    pub(crate) target_tag: String,
    /// Whether the target release differs from `current`.
    pub(crate) changes_version: bool,
    /// Whether `target` is newer than `current`.
    pub(crate) update_available: bool,
    /// The release target triple for this host.
    pub(crate) host_target: &'static str,
}

/// Why `phux update` will not perform an update itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A Nix store path. Read-only by construction.
    ImmutableStore,
    /// Homebrew or Cargo owns these files.
    PackageManaged,
    /// No recognized layout. Refused rather than overwritten.
    UnknownSource,
}

impl Refusal {
    /// The refusal, if any, implied by an install's source.
    const fn of(source: InstallSource) -> Option<Self> {
        match source {
            InstallSource::DirectRelease => None,
            InstallSource::Nix => Some(Self::ImmutableStore),
            InstallSource::Homebrew | InstallSource::Cargo => Some(Self::PackageManaged),
            InstallSource::Unknown => Some(Self::UnknownSource),
        }
    }

    /// The stable `--json` error code.
    const fn code(self) -> &'static str {
        match self {
            Self::ImmutableStore => codes::UPDATE_IMMUTABLE_STORE,
            Self::PackageManaged => codes::UPDATE_PACKAGE_MANAGED,
            Self::UnknownSource => codes::UPDATE_SOURCE_UNSUPPORTED,
        }
    }

    /// The refusal message for `install`.
    fn message(self, install: &Install) -> String {
        let path = install.executable.display();
        match self {
            Self::ImmutableStore => {
                format!("{path} is a read-only Nix store path; phux will not modify it")
            }
            Self::PackageManaged => format!(
                "{path} is managed by {}; phux will not modify files another \
                 package manager owns",
                install.source.as_str()
            ),
            Self::UnknownSource => format!(
                "{path} is not a recognized phux install location, so phux \
                 will not overwrite it"
            ),
        }
    }

    /// The remedy: the exact native command, or the way to get to a layout
    /// `phux update` can maintain.
    fn remedy(install: &Install) -> String {
        install.native_command().unwrap_or_else(|| {
            let mut remedy = String::from(
                "phux updates installs under $PHUX_INSTALL_DIR, ~/.local/bin, ~/bin, \
                 /usr/local/bin, or /opt/phux/bin.",
            );
            if install.unknown_reason == Some(UnknownReason::BuildDirectory) {
                remedy.push_str(
                    "\nThis looks like a build directory; rebuild the checkout instead \
                     (`just install-dev`).",
                );
            } else {
                remedy.push_str(
                    "\nReinstall into one of those with the curl installer in \
                     docs/INSTALL.md, or set PHUX_INSTALL_DIR to this directory.",
                );
            }
            remedy
        })
    }
}

/// The side effects `phux update` performs, gathered behind one value so
/// tests can substitute every one of them.
///
/// This is the same injection discipline the rest of the crate uses for
/// side-effecting verbs: the decision logic is pure and the effects are
/// values. Nothing in the test suite performs a real download.
pub(crate) struct UpdateEnv<'a> {
    /// Where release metadata and artifacts come from.
    pub(crate) releases: &'a dyn ReleaseSource,
    /// The live-server handoff — `phux upgrade`'s primitive, injected so the
    /// install path can be exercised without a server.
    pub(crate) handoff: &'a dyn Fn() -> Handoff,
    /// The install this run operates on.
    pub(crate) install: Install,
}

impl std::fmt::Debug for UpdateEnv<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateEnv")
            .field("releases", &self.releases)
            .field("install", &self.install)
            .finish_non_exhaustive()
    }
}

/// How the live-server handoff went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// The server acked and is re-execing; panes survive.
    Upgrading,
    /// No server was running, so there was nothing to hand off.
    NoServer,
    /// The server refused.
    Refused(String),
    /// The handoff was not attempted (`--no-restart`).
    Skipped,
    /// The handoff failed for a transport reason.
    Failed(String),
}

impl Handoff {
    /// The stable `--json` token.
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Upgrading => "upgrading",
            Self::NoServer => "no_server",
            Self::Refused(_) => "refused",
            Self::Skipped => "skipped",
            Self::Failed(_) => "failed",
        }
    }

    /// The human detail, when there is one.
    fn detail(&self) -> Option<&str> {
        match self {
            Self::Refused(message) | Self::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// What actually happened, as reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// `--check`: nothing was touched.
    Checked,
    /// Already on the target release; nothing to do.
    UpToDate,
    /// `--dry-run`: downloaded and verified, installed nothing.
    Planned,
    /// Binaries replaced.
    Installed,
    /// Previous binaries restored.
    RolledBack,
}

impl Action {
    /// The stable `--json` token.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Checked => "checked",
            Self::UpToDate => "up-to-date",
            Self::Planned => "planned",
            Self::Installed => "installed",
            Self::RolledBack => "rolled-back",
        }
    }
}

/// One completed run, ready to render as prose or JSON.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    /// What happened.
    pub(crate) action: Action,
    /// The decision this run was made from.
    pub(crate) plan: Plan,
    /// The artifact, once one was resolved.
    pub(crate) artifact: Option<Artifact>,
    /// The verified SHA-256, once the download was checked.
    pub(crate) digest: Option<String>,
    /// The binaries that changed.
    pub(crate) binaries: Vec<String>,
    /// Where the previous binaries were saved.
    pub(crate) backup: Option<PathBuf>,
    /// How the live-server handoff went.
    pub(crate) handoff: Option<Handoff>,
}

impl Outcome {
    /// The stable machine document.
    ///
    /// `schema_version` is the universal CLI convention (ADR-0071 freezes the
    /// shape); every optional member is present as `null` rather than absent,
    /// so a consumer can index it unconditionally.
    pub(crate) fn document(&self) -> serde_json::Value {
        let install = &self.plan.install;
        serde_json::json!({
            "schema_version": DOCUMENT_SCHEMA_VERSION,
            "action": self.action.as_str(),
            "current_version": self.plan.current.to_string(),
            "latest_version": self.plan.latest_tag,
            "target_version": self.plan.target_tag,
            "update_available": self.plan.update_available,
            "install": {
                "source": install.source.as_str(),
                "executable": install.executable.display().to_string(),
                "mutable": install.source.is_mutable(),
                "native_command": install.native_command(),
            },
            "platform": {
                "target": self.plan.host_target,
            },
            "artifact": self.artifact.as_ref().map(|artifact| serde_json::json!({
                "archive": artifact.archive,
                "archive_url": artifact.archive_url,
                "checksum_url": artifact.checksum_url,
                "sha256": self.digest,
            })),
            "binaries": self.binaries,
            "backup": self.backup.as_ref().map(|path| path.display().to_string()),
            "server_handoff": self.handoff.as_ref().map(|handoff| serde_json::json!({
                "result": handoff.as_str(),
                "detail": handoff.detail(),
            })),
        })
    }

    /// The human view, one line per fact.
    pub(crate) fn lines(&self) -> Vec<String> {
        let install = &self.plan.install;
        let mut lines = vec![
            format!("current:  {}", self.plan.current),
            format!("latest:   {}", self.plan.latest_tag),
            format!(
                "source:   {} ({})",
                install.source.as_str(),
                install.executable.display()
            ),
        ];
        match self.action {
            Action::Checked => {
                lines.push(if self.plan.update_available {
                    format!(
                        "an update is available: {} -> {}",
                        self.plan.current, self.plan.target_tag
                    )
                } else {
                    "already on the latest release".to_owned()
                });
                // The install source is always spoken to, update available or
                // not: "phux will not maintain this install" is exactly the
                // fact a user checking for updates needs, and learning it
                // only at the moment of failure is what this verb exists to
                // avoid.
                if let Some(refusal) = Refusal::of(install.source) {
                    lines.push(refusal.message(install));
                    lines.extend(
                        Refusal::remedy(install)
                            .lines()
                            .map(|line| format!("  {line}")),
                    );
                } else if self.plan.update_available {
                    lines.push("run `phux update` to install it".to_owned());
                }
            }
            Action::UpToDate => lines.push(format!("already on {}", self.plan.target_tag)),
            Action::Planned => {
                if let Some(artifact) = self.artifact.as_ref() {
                    lines.push(format!("verified: {}", artifact.archive));
                }
                if let Some(digest) = self.digest.as_ref() {
                    lines.push(format!("sha256:   {digest}"));
                }
                lines.push(format!(
                    "dry run: would install {} and replace {}",
                    self.plan.target_tag,
                    self.binaries.join(", ")
                ));
            }
            Action::Installed => {
                if let Some(digest) = self.digest.as_ref() {
                    lines.push(format!("sha256:   {digest} (verified)"));
                }
                lines.push(format!(
                    "installed {}: {}",
                    self.plan.target_tag,
                    self.binaries.join(", ")
                ));
                if let Some(backup) = self.backup.as_ref() {
                    lines.push(format!(
                        "previous binaries saved in {} (`phux update --rollback` restores them)",
                        backup.display()
                    ));
                }
            }
            Action::RolledBack => lines.push(format!(
                "restored {}: {}",
                self.plan.target_tag,
                self.binaries.join(", ")
            )),
        }
        if let Some(handoff) = self.handoff.as_ref() {
            lines.push(match handoff {
                Handoff::Upgrading => "server upgrading in place; sessions preserved".to_owned(),
                Handoff::NoServer => {
                    "no server was running; the next `phux` starts the new binary".to_owned()
                }
                Handoff::Skipped => {
                    "server left alone (--no-restart); run `phux upgrade` when ready".to_owned()
                }
                Handoff::Refused(message) => format!(
                    "the running server refused the handoff: {message}\n  \
                     it keeps serving the old image; run `phux upgrade` to retry"
                ),
                Handoff::Failed(message) => format!(
                    "the handoff could not be delivered: {message}\n  \
                     the new binary is installed; run `phux upgrade` to retry"
                ),
            });
        }
        lines
    }
}

/// Build the plan: what is installed, what is published, and what would
/// change. Pure given the resolved inputs.
pub(crate) fn plan(
    install: Install,
    current: Version,
    latest_tag: &str,
    requested: Option<&str>,
    host_target: &'static str,
) -> Result<Plan, UpdateError> {
    release::validate_tag(latest_tag)?;
    let target_tag = requested.unwrap_or(latest_tag).to_owned();
    let target = release::validate_tag(&target_tag)?;
    Ok(Plan {
        install,
        current,
        latest_tag: latest_tag.to_owned(),
        target_tag,
        changes_version: target != current,
        update_available: target > current,
        host_target,
    })
}

/// Run one `phux update` against injected effects.
///
/// Split out from [`run_update`] so the whole flow — resolve, download,
/// verify, stage, replace, hand off — is reachable from a test with a fake
/// [`ReleaseSource`] and a scratch bin directory.
pub(crate) fn execute(opts: &UpdateOpts, env: &UpdateEnv<'_>) -> Result<Outcome, UpdateError> {
    let install = env.install.clone();
    let current = Version::current().ok_or_else(|| {
        UpdateError::InvalidTag(format!(
            "this build's version (`{}`) is not a release version",
            env!("CARGO_PKG_VERSION")
        ))
    })?;

    if opts.rollback {
        return rollback(opts, install, current, env);
    }

    let host_target = release::host_target()?;
    // A caller-supplied tag is validated before anything reaches the network:
    // a typo should cost a millisecond and one clear message, not a round
    // trip, and `--check --version <typo>` must be diagnosable offline.
    if let Some(tag) = opts.tag.as_deref() {
        release::validate_tag(tag)?;
    }
    let latest_tag = env.releases.latest_tag()?;
    let plan = plan(
        install,
        current,
        &latest_tag,
        opts.tag.as_deref(),
        host_target,
    )?;

    if opts.check {
        return Ok(Outcome {
            action: Action::Checked,
            plan,
            artifact: None,
            digest: None,
            binaries: Vec::new(),
            backup: None,
            handoff: None,
        });
    }

    if !plan.changes_version {
        return Ok(Outcome {
            action: Action::UpToDate,
            plan,
            artifact: None,
            digest: None,
            binaries: Vec::new(),
            backup: None,
            handoff: None,
        });
    }

    install_release(opts, env, plan)
}

/// Download, verify, and (unless `--dry-run`) replace.
fn install_release(
    opts: &UpdateOpts,
    env: &UpdateEnv<'_>,
    plan: Plan,
) -> Result<Outcome, UpdateError> {
    let bin_dir = plan
        .install
        .bin_dir()
        .ok_or_else(|| {
            UpdateError::Install(format!(
                "{} has no parent directory to install into",
                plan.install.executable.display()
            ))
        })?
        .to_path_buf();

    let artifact = Artifact::new(&plan.target_tag, plan.host_target);
    let staging = apply::Staging::create(&bin_dir)?;

    // Both downloads land in the staging directory, which is removed on every
    // exit path including a failure.
    let archive_path = staging.path().join(&artifact.archive);
    let sidecar_path = staging.path().join(format!("{}.sha256", artifact.archive));
    env.releases
        .download(&artifact.archive_url, &archive_path)?;
    env.releases
        .download(&artifact.checksum_url, &sidecar_path)?;

    let sidecar = std::fs::read_to_string(&sidecar_path).map_err(|err| {
        UpdateError::Checksum(format!("could not read the .sha256 sidecar: {err}"))
    })?;

    // THE GATE. Nothing below this line runs unless the published digest and
    // the downloaded bytes agree.
    let digest = apply::verify_archive(&archive_path, &sidecar, &artifact.archive)?;

    let staged = apply::unpack_verified(&archive_path, &artifact.stage, staging.path())?;

    if opts.dry_run {
        let would_replace: Vec<String> = apply::RELEASE_BINARIES
            .iter()
            .filter(|name| **name == "phux" || bin_dir.join(name).exists())
            .map(|name| (*name).to_owned())
            .collect();
        return Ok(Outcome {
            action: Action::Planned,
            plan,
            artifact: Some(artifact),
            digest: Some(digest),
            binaries: would_replace,
            backup: None,
            handoff: None,
        });
    }

    let replaced = apply::replace_binaries(&bin_dir, &staged, &plan.current.to_string())?;
    let handoff = if opts.no_restart {
        Handoff::Skipped
    } else {
        (env.handoff)()
    };

    Ok(Outcome {
        action: Action::Installed,
        plan,
        artifact: Some(artifact),
        digest: Some(digest),
        binaries: replaced.binaries,
        backup: Some(replaced.backup),
        handoff: Some(handoff),
    })
}

/// Restore the binaries saved by the previous update.
///
/// The handoff runs here too, and for the same reason it runs after an
/// install: the point of a rollback is that the *running* server goes back to
/// the previous image, not just the file on disk. `--no-restart` suppresses
/// it exactly as it does on the way forward.
fn rollback(
    opts: &UpdateOpts,
    install: Install,
    current: Version,
    env: &UpdateEnv<'_>,
) -> Result<Outcome, UpdateError> {
    let bin_dir = install
        .bin_dir()
        .ok_or_else(|| {
            UpdateError::NoBackup(format!(
                "{} has no parent directory to look for a backup in",
                install.executable.display()
            ))
        })?
        .to_path_buf();
    let restored = apply::rollback(&bin_dir)?;
    let restored_tag = format!("v{}", restored.version);
    let target = Version::parse(&restored.version).unwrap_or(current);
    let plan = Plan {
        install,
        current,
        latest_tag: restored_tag.clone(),
        target_tag: restored_tag,
        changes_version: target != current,
        update_available: false,
        host_target: release::host_target().unwrap_or("unknown"),
    };
    let handoff = if opts.no_restart {
        Handoff::Skipped
    } else {
        (env.handoff)()
    };
    Ok(Outcome {
        action: Action::RolledBack,
        plan,
        artifact: None,
        digest: None,
        binaries: restored.binaries,
        backup: None,
        handoff: Some(handoff),
    })
}

/// `phux update` — check for, download, verify, and install a newer phux,
/// then hand a running server off to it.
pub(crate) fn run_update(opts: &UpdateOpts, socket: Option<PathBuf>) -> ExitCode {
    let probe = match source::Probe::from_process() {
        Ok(probe) => probe,
        Err(err) => {
            return json_err::emit(
                opts.json,
                &CliError::new(
                    codes::INTERNAL_ERROR,
                    format!("could not resolve this binary's own path: {err}"),
                    "phux update needs to know which file it would replace".to_owned(),
                ),
                EXIT_FAILURE,
            );
        }
    };
    let install = source::detect(&probe);
    let socket_path = socket.unwrap_or_else(phux_server::runtime::default_socket_path);

    // A mutation is refused before any network traffic when the install is
    // one phux does not own. `--check` is exempt: reporting is always
    // allowed, and naming the native command is the whole point of it.
    if !opts.check
        && let Some(refusal) = Refusal::of(install.source)
    {
        let err = CliError::new(
            refusal.code(),
            refusal.message(&install),
            Refusal::remedy(&install),
        );
        return json_err::emit(opts.json, &err, EXIT_USAGE);
    }

    let handoff = || live_handoff(&socket_path);
    let env = UpdateEnv {
        releases: &release::NetworkReleaseSource,
        handoff: &handoff,
        install,
    };

    match execute(opts, &env) {
        Ok(outcome) => {
            if opts.json {
                match serde_json::to_string_pretty(&outcome.document()) {
                    Ok(rendered) => outln!("{rendered}"),
                    Err(err) => {
                        return json_err::emit(
                            true,
                            &CliError::new(
                                codes::JSON_SERIALIZE,
                                format!("could not render the update document: {err}"),
                                "report this at https://github.com/phall1/phux/issues".to_owned(),
                            ),
                            EXIT_FAILURE,
                        );
                    }
                }
            } else {
                for line in outcome.lines() {
                    outln!("{line}");
                }
            }
            ExitCode::from(EXIT_SUCCESS)
        }
        Err(err) => err.report(opts.json),
    }
}

/// Ask the running server to re-exec, through `phux upgrade`'s own primitive.
fn live_handoff(socket_path: &std::path::Path) -> Handoff {
    use phux_client::attach::AttachError;

    match super::upgrade::request_upgrade(socket_path) {
        Ok(super::upgrade::UpgradeAck::Upgrading) => Handoff::Upgrading,
        Ok(
            super::upgrade::UpgradeAck::Refused(message)
            | super::upgrade::UpgradeAck::Unexpected(message),
        ) => Handoff::Refused(message),
        Err(AttachError::Io(err))
            if matches!(
                err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Handoff::NoServer
        }
        Err(err) => Handoff::Failed(err.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::apply::BACKUP_DIR;
    use super::release::{Artifact, ReleaseSource, Version};
    use super::source::{Install, InstallSource};
    use super::{Action, Handoff, Refusal, UpdateEnv, UpdateError, UpdateOpts, execute, plan};

    const TARGET: &str = "aarch64-apple-darwin";

    /// A [`ReleaseSource`] backed by a map of URL to bytes. Nothing in this
    /// module's tests touches the network.
    #[derive(Debug)]
    struct FakeReleases {
        latest: String,
        files: HashMap<String, Vec<u8>>,
        downloads: RefCell<Vec<String>>,
    }

    impl FakeReleases {
        fn new(latest: &str) -> Self {
            Self {
                latest: latest.to_owned(),
                files: HashMap::new(),
                downloads: RefCell::new(Vec::new()),
            }
        }

        fn serve(&mut self, url: &str, bytes: Vec<u8>) {
            self.files.insert(url.to_owned(), bytes);
        }
    }

    impl ReleaseSource for FakeReleases {
        fn latest_tag(&self) -> Result<String, UpdateError> {
            Ok(self.latest.clone())
        }

        fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
            self.downloads.borrow_mut().push(url.to_owned());
            let bytes = self
                .files
                .get(url)
                .ok_or_else(|| UpdateError::Fetch(format!("404 for {url}")))?;
            fs::write(dest, bytes).map_err(|err| {
                UpdateError::Fetch(format!("could not write {}: {err}", dest.display()))
            })
        }
    }

    /// A scratch directory removed on drop.
    #[derive(Debug)]
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "phux-update-flow-{tag}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn opts() -> UpdateOpts {
        UpdateOpts {
            check: false,
            dry_run: false,
            tag: None,
            rollback: false,
            no_restart: true,
            json: false,
        }
    }

    fn install_at(path: &Path, source: InstallSource) -> Install {
        Install {
            source,
            executable: path.to_owned(),
            nixos: false,
            unknown_reason: None,
        }
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// Build a release tarball plus its sidecar and serve both at the URLs
    /// the real artifact naming would use.
    fn publish(fake: &mut FakeReleases, workdir: &Path, tag: &str) -> Artifact {
        let artifact = Artifact::new(tag, TARGET);
        let build = workdir.join("build").join(&artifact.stage);
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("phux"), format!("phux {tag}")).unwrap();
        fs::set_permissions(build.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(build.join("phux-mcp"), format!("phux-mcp {tag}")).unwrap();
        fs::set_permissions(build.join("phux-mcp"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(build.join("README.md"), b"readme").unwrap();
        fs::write(build.join("LICENSE-MIT"), b"mit").unwrap();
        fs::write(build.join("LICENSE-APACHE"), b"apache").unwrap();

        let archive = workdir.join(&artifact.archive);
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(workdir.join("build"))
            .arg(&artifact.stage)
            .status()
            .unwrap();
        assert!(status.success());

        let digest = super::apply::sha256_file(&archive).unwrap();
        let bytes = fs::read(&archive).unwrap();
        fake.serve(&artifact.archive_url, bytes);
        fake.serve(
            &artifact.checksum_url,
            format!("{digest}  {}\n", artifact.archive).into_bytes(),
        );
        artifact
    }

    /// A bin directory holding a "current" install.
    fn seed_bin(scratch: &Scratch) -> PathBuf {
        let bin = scratch.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("phux"), b"old phux").unwrap();
        fs::set_permissions(bin.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(bin.join("phux-mcp"), b"old phux-mcp").unwrap();
        fs::set_permissions(bin.join("phux-mcp"), fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    #[test]
    fn plan_reports_an_available_update_and_names_the_source() {
        let install = install_at(
            Path::new("/home/ada/.local/bin/phux"),
            InstallSource::DirectRelease,
        );
        let plan = plan(install, version("0.12.1"), "v0.13.0", None, TARGET).unwrap();
        assert!(plan.update_available);
        assert!(plan.changes_version);
        assert_eq!(plan.target_tag, "v0.13.0");
    }

    #[test]
    fn plan_treats_an_explicit_older_tag_as_a_change_but_not_an_update() {
        let install = install_at(
            Path::new("/home/ada/.local/bin/phux"),
            InstallSource::DirectRelease,
        );
        let plan = plan(
            install,
            version("0.12.1"),
            "v0.13.0",
            Some("v0.11.0"),
            TARGET,
        )
        .unwrap();
        assert!(!plan.update_available);
        assert!(plan.changes_version);
        assert_eq!(plan.target_tag, "v0.11.0");
    }

    #[test]
    fn plan_refuses_a_tag_that_is_not_a_release_tag() {
        let install = install_at(
            Path::new("/home/ada/.local/bin/phux"),
            InstallSource::DirectRelease,
        );
        let err = plan(
            install,
            version("0.12.1"),
            "v0.13.0",
            Some("nightly"),
            TARGET,
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::InvalidTag(_)), "{err:?}");
    }

    #[test]
    fn refusals_cover_every_non_mutable_source_and_carry_a_command() {
        for (source, expected) in [
            (InstallSource::Nix, Refusal::ImmutableStore),
            (InstallSource::Homebrew, Refusal::PackageManaged),
            (InstallSource::Cargo, Refusal::PackageManaged),
            (InstallSource::Unknown, Refusal::UnknownSource),
        ] {
            let refusal = Refusal::of(source).unwrap();
            assert_eq!(refusal, expected);
            let install = install_at(Path::new("/somewhere/phux"), source);
            assert!(!refusal.message(&install).is_empty());
            assert!(!Refusal::remedy(&install).is_empty());
        }
        assert!(Refusal::of(InstallSource::DirectRelease).is_none());
    }

    #[test]
    fn check_reports_without_downloading_anything() {
        let fake = FakeReleases::new("v0.13.0");
        let install = install_at(
            Path::new("/home/ada/.local/bin/phux"),
            InstallSource::DirectRelease,
        );
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let outcome = execute(
            &UpdateOpts {
                check: true,
                ..opts()
            },
            &env,
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Checked);
        assert_eq!(outcome.plan.latest_tag, "v0.13.0");
        assert!(outcome.artifact.is_none());
        assert!(
            fake.downloads.borrow().is_empty(),
            "--check must not download"
        );

        let doc = outcome.document();
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["action"], "checked");
        assert_eq!(doc["install"]["source"], "direct-release");
        assert_eq!(doc["install"]["mutable"], true);
        assert_eq!(doc["latest_version"], "v0.13.0");
        assert_eq!(doc["artifact"], serde_json::Value::Null);
        assert_eq!(doc["server_handoff"], serde_json::Value::Null);
    }

    #[test]
    fn check_on_a_package_managed_install_names_the_native_command() {
        let fake = FakeReleases::new("v0.13.0");
        let install = install_at(
            Path::new("/opt/homebrew/Cellar/phux/0.12.1/bin/phux"),
            InstallSource::Homebrew,
        );
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let outcome = execute(
            &UpdateOpts {
                check: true,
                ..opts()
            },
            &env,
        )
        .unwrap();
        let doc = outcome.document();
        assert_eq!(doc["install"]["mutable"], false);
        assert_eq!(
            doc["install"]["native_command"],
            "brew upgrade phall1/tap/phux"
        );
        assert!(
            outcome
                .lines()
                .iter()
                .any(|line| line.contains("brew upgrade phall1/tap/phux")),
            "the prose view must print the command too: {:?}",
            outcome.lines()
        );
    }

    #[test]
    fn dry_run_downloads_and_verifies_but_installs_nothing() {
        let scratch = Scratch::new("dry-run");
        let bin = seed_bin(&scratch);
        let mut fake = FakeReleases::new("v9.9.9");
        publish(&mut fake, scratch.path(), "v9.9.9");

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let outcome = execute(
            &UpdateOpts {
                dry_run: true,
                ..opts()
            },
            &env,
        )
        .unwrap();

        assert_eq!(outcome.action, Action::Planned);
        assert!(outcome.digest.is_some(), "the checksum must be verified");
        assert_eq!(outcome.binaries, vec!["phux-mcp", "phux"]);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert!(!bin.join(BACKUP_DIR).exists());
        // The staging directory is gone even though nothing was installed.
        let leftovers: Vec<_> = fs::read_dir(&bin)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".phux-update-"))
            .collect();
        assert!(leftovers.is_empty(), "staging leaked: {leftovers:?}");
    }

    #[test]
    fn a_full_update_installs_verifies_and_hands_off() {
        let scratch = Scratch::new("full");
        let bin = seed_bin(&scratch);
        let mut fake = FakeReleases::new("v9.9.9");
        let artifact = publish(&mut fake, scratch.path(), "v9.9.9");

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Upgrading;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let outcome = execute(
            &UpdateOpts {
                no_restart: false,
                ..opts()
            },
            &env,
        )
        .unwrap();

        assert_eq!(outcome.action, Action::Installed);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");
        assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"phux-mcp v9.9.9");
        assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

        let doc = outcome.document();
        assert_eq!(doc["action"], "installed");
        assert_eq!(doc["target_version"], "v9.9.9");
        assert_eq!(doc["artifact"]["archive"], artifact.archive);
        assert_eq!(doc["server_handoff"]["result"], "upgrading");
        assert!(doc["artifact"]["sha256"].is_string());
        assert!(doc["backup"].is_string());

        // Both artifact URLs were fetched, checksum included.
        let fetched = fake.downloads.borrow().clone();
        assert!(fetched.contains(&artifact.archive_url));
        assert!(fetched.contains(&artifact.checksum_url));
    }

    #[test]
    fn a_tampered_archive_is_refused_and_nothing_is_replaced() {
        let scratch = Scratch::new("tamper");
        let bin = seed_bin(&scratch);
        let mut fake = FakeReleases::new("v9.9.9");
        let artifact = publish(&mut fake, scratch.path(), "v9.9.9");
        // Swap the archive after the sidecar was published.
        fake.serve(&artifact.archive_url, b"totally different bytes".to_vec());

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let err = execute(&opts(), &env).unwrap_err();
        assert!(
            matches!(err, UpdateError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.code(), "update_checksum_mismatch");
        assert!(err.to_string().contains("checksum mismatch"));
        // The install is untouched.
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert!(!bin.join(BACKUP_DIR).exists());
    }

    #[test]
    fn a_missing_artifact_fails_without_touching_the_install() {
        let scratch = Scratch::new("missing");
        let bin = seed_bin(&scratch);
        let fake = FakeReleases::new("v9.9.9");

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let err = execute(&opts(), &env).unwrap_err();
        assert!(matches!(err, UpdateError::Fetch(_)), "{err:?}");
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    }

    #[test]
    fn an_install_already_on_the_target_release_does_nothing() {
        let scratch = Scratch::new("current");
        let bin = seed_bin(&scratch);
        let current = format!("v{}", Version::current().unwrap());
        let fake = FakeReleases::new(&current);

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Skipped;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        let outcome = execute(&opts(), &env).unwrap();
        assert_eq!(outcome.action, Action::UpToDate);
        assert!(!outcome.plan.update_available);
        assert!(fake.downloads.borrow().is_empty());
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    }

    #[test]
    fn rollback_restores_the_previous_release_and_hands_off_again() {
        let scratch = Scratch::new("rollback");
        let bin = seed_bin(&scratch);
        let mut fake = FakeReleases::new("v9.9.9");
        publish(&mut fake, scratch.path(), "v9.9.9");

        let install = install_at(&bin.join("phux"), InstallSource::DirectRelease);
        let handoff = || Handoff::Upgrading;
        let env = UpdateEnv {
            releases: &fake,
            handoff: &handoff,
            install,
        };
        execute(&opts(), &env).unwrap();
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");

        let outcome = execute(
            &UpdateOpts {
                rollback: true,
                ..opts()
            },
            &env,
        )
        .unwrap();
        assert_eq!(outcome.action, Action::RolledBack);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"old phux-mcp");
        assert_eq!(outcome.document()["action"], "rolled-back");
        // `opts()` sets --no-restart, so the running server is left alone on
        // the way back exactly as it is on the way forward.
        assert_eq!(outcome.handoff, Some(Handoff::Skipped));
        assert_eq!(outcome.document()["server_handoff"]["result"], "skipped");

        // Without --no-restart the rollback hands the live server back to the
        // restored image, which is the whole point of rolling back.
        execute(&opts(), &env).unwrap();
        let outcome = execute(
            &UpdateOpts {
                rollback: true,
                no_restart: false,
                ..opts()
            },
            &env,
        )
        .unwrap();
        assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

        // A second rollback has nothing left to restore and says so.
        let err = execute(
            &UpdateOpts {
                rollback: true,
                ..opts()
            },
            &env,
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::NoBackup(_)), "{err:?}");
        assert_eq!(err.code(), "update_no_backup");
    }

    /// Every failure carries a code from the closed vocabulary, a non-empty
    /// remedy, and one of the documented exit codes.
    #[test]
    fn every_failure_is_loud() {
        let failures = [
            UpdateError::InvalidTag("t".to_owned()),
            UpdateError::UnsupportedPlatform("p".to_owned()),
            UpdateError::Fetch("f".to_owned()),
            UpdateError::Checksum("c".to_owned()),
            UpdateError::ChecksumMismatch {
                expected: "a".to_owned(),
                actual: "b".to_owned(),
                archive: "x.tar.gz".to_owned(),
            },
            UpdateError::Archive("a".to_owned()),
            UpdateError::Install("i".to_owned()),
            UpdateError::NoBackup("n".to_owned()),
        ];
        for failure in &failures {
            assert!(failure.code().starts_with("update_"), "{failure:?}");
            assert!(!failure.to_string().is_empty(), "{failure:?}");
            assert!(!failure.remedy().is_empty(), "{failure:?}");
            assert!(
                matches!(failure.exit_code(), 1 | 2),
                "{failure:?} uses an undocumented exit code"
            );
        }
    }

    /// The handoff tokens are the frozen `--json` vocabulary.
    #[test]
    fn handoff_tokens_are_stable() {
        assert_eq!(Handoff::Upgrading.as_str(), "upgrading");
        assert_eq!(Handoff::NoServer.as_str(), "no_server");
        assert_eq!(Handoff::Skipped.as_str(), "skipped");
        assert_eq!(Handoff::Refused(String::new()).as_str(), "refused");
        assert_eq!(Handoff::Failed(String::new()).as_str(), "failed");
        assert_eq!(Handoff::Refused("why".to_owned()).detail(), Some("why"));
        assert_eq!(Handoff::Upgrading.detail(), None);
    }
}
