//! `phux update` — the one-command path from the release phux is running to
//! the release phux publishes. The compatibility unit is the release
//! (ADR-0071: the wire keeps its own `0.x` line), so moving a fleet between
//! releases has to be one command.
//!
//! Trust boundary:
//!
//! * The download's digest must equal the channel manifest's (or, for a
//!   `--version` pin the manifest does not name, its `.sha256` sidecar's)
//!   before anything is unpacked; a mismatch installs nothing.
//! * Nothing downloaded is executed to decide whether to install it; the
//!   archive is data, validated before and after unpacking.
//! * Replacement is atomic and permission-preserving (see [`apply`]).
//! * An install phux does not own (Homebrew, Cargo, Nix) gets its native
//!   command instead, and an unrecognized install is refused.
//!
//! `phux upgrade` remains the low-level primitive that asks a running server to
//! re-exec the binary on disk; `phux update` puts a new binary there and then
//! calls it so live panes survive.

pub(crate) mod apply;
pub(crate) mod channel;
pub(crate) mod release;
pub(crate) mod source;

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client_runtime::release::{
    self as shared, Asset, Build, Channel, Entry, Product, Version,
};
use usage::Args;

use self::channel::ChannelArg;
use self::release::ReleaseSource;
use self::source::{Install, InstallSource, UnknownReason};
use super::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_SUCCESS, EXIT_USAGE};

/// Version of the `phux update --json` document. Version 2 removes the
/// retired native app's update report; additive fields do not bump it.
pub(crate) const DOCUMENT_SCHEMA_VERSION: u8 = 2;

/// Everything that can go wrong between "there is a newer release" and "it is
/// installed", with the failure kept separate from how it is reported.
#[derive(Debug)]
pub(crate) enum UpdateError {
    /// A release tag that is not `vMAJOR.MINOR.PATCH`.
    InvalidTag(String),
    /// This OS/architecture has no published release artifact.
    UnsupportedPlatform(String),
    /// The release manifest was unreachable or invalid, or an artifact could
    /// not be downloaded.
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
                "pass a tag from https://github.com/no-phux/phux/releases, like `--version v1.2.3`"
                    .to_owned()
            }
            Self::UnsupportedPlatform(_) => {
                "build from source: see docs/INSTALL.md#from-source".to_owned()
            }
            Self::Fetch(_) => "check network access to github.com, then retry; \
                 `phux update --check` re-reads the release manifest, and \
                 `--version vX.Y.Z` installs a pinned release without it"
                .to_owned(),
            Self::Checksum(_) | Self::ChecksumMismatch { .. } => {
                "nothing was installed. Re-run to download again; if it \
                 mismatches a second time, do not install this artifact by \
                 hand — report it at https://github.com/no-phux/phux/issues"
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
    reason = "a usage flag struct; each bool is one independent CLI switch, and \
              collapsing them into an enum would change the frozen grammar"
)]
pub(crate) struct UpdateOpts {
    /// Report the current and latest release and the install source, then
    /// stop. Changes nothing and never downloads an archive.
    #[usage(long, conflicts("--dry-run", "--rollback"))]
    pub(crate) check: bool,

    /// Do everything except the replacement: resolve, download, and verify
    /// the checksum, then report what would have been installed.
    #[usage(long, conflicts("--rollback"))]
    pub(crate) dry_run: bool,

    /// Install this release tag instead of the latest one. Accepts any tag
    /// from the releases page, including an older one (a downgrade).
    /// Stable-only: omit this when following `--channel next`.
    #[usage(long = "version", value_name = "TAG", conflicts("--rollback"))]
    pub(crate) tag: Option<String>,

    /// Release channel to follow. `stable` (also `latest`) is the default
    /// (`vX.Y.Z`). `next` tracks green `main` via the moving prerelease.
    #[usage(long, value_enum, value_name = "CHANNEL", conflicts("--rollback"))]
    pub(crate) channel: Option<ChannelArg>,

    /// Restore the binaries saved by the previous `phux update`.
    #[usage(long)]
    pub(crate) rollback: bool,

    /// Replace the binaries but do not ask a running server to re-exec.
    /// Live panes keep the old image until the server is upgraded or
    /// restarted.
    #[usage(long)]
    pub(crate) no_restart: bool,

    /// When the running server is too old to hand off while agent sessions
    /// exist, close them and retry without asking (see `phux upgrade
    /// --close-agent-sessions`). Panes and agent processes keep running.
    #[usage(long, conflicts("--check", "--dry-run", "--no-restart"))]
    pub(crate) close_agent_sessions: bool,

    /// Emit the stable, versioned JSON document on stdout instead of the
    /// human view. On failure, stdout stays empty and stderr carries one
    /// JSON error object.
    #[usage(long)]
    pub(crate) json: bool,
}

/// What `phux update` decided before it did anything.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// Where this binary came from.
    pub(crate) install: Install,
    /// The version this binary was built as.
    pub(crate) current: Version,
    /// Channel this run will follow.
    pub(crate) channel: Channel,
    /// Git SHA baked into this binary, when it is a next build.
    pub(crate) current_sha: Option<String>,
    /// What the channel's manifest names, when it was read.
    pub(crate) latest: Option<Entry>,
    /// What this run installs: the manifest's build, or a `--version` pin.
    pub(crate) target: Build,
    /// Whether the target differs from what is installed.
    pub(crate) changes_version: bool,
    /// Whether the target is an update over what is installed.
    pub(crate) update_available: bool,
    /// The release target triple for this host.
    pub(crate) host_target: &'static str,
}

impl Plan {
    /// The manifest entry for the target, when the manifest names it. A pin
    /// to any other release is verified against its own sidecar.
    fn vouched(&self) -> Option<&Entry> {
        self.latest
            .as_ref()
            .filter(|latest| latest.build == self.target)
    }

    /// The newest release on this channel, as a tag.
    fn latest_tag(&self) -> String {
        self.latest
            .as_ref()
            .map_or(&self.target, |latest| &latest.build)
            .tag()
    }

    /// The commit at the head of next, when following it.
    fn latest_sha(&self) -> Option<&str> {
        self.latest
            .as_ref()
            .filter(|_| self.channel == Channel::Next)
            .map(|latest| latest.sha.as_str())
    }
}

/// The protocol `major.minor` this build speaks.
fn current_protocol() -> String {
    format!(
        "{}.{}",
        phux_protocol::PROTOCOL_VERSION.major,
        phux_protocol::PROTOCOL_VERSION.minor
    )
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

/// The side effects `phux update` performs, gathered behind one value so tests
/// substitute every one of them (no test performs a real download).
pub(crate) struct UpdateEnv<'a> {
    /// Where release metadata and artifacts come from.
    pub(crate) releases: &'a dyn ReleaseSource,
    /// The live-server handoff — `phux upgrade`'s primitive, injected so the
    /// install path can be exercised without a server.
    pub(crate) handoff: &'a dyn Fn() -> Handoff,
    /// The install this run operates on.
    pub(crate) install: Install,
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
    /// The tarball, once one was resolved.
    pub(crate) artifact: Option<Asset>,
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
            "channel": self.plan.channel.as_str(),
            "current_version": self.plan.current.to_string(),
            "current_sha": self.plan.current_sha,
            "latest_version": self.plan.latest_tag(),
            "latest_sha": self.plan.latest_sha(),
            "target_version": self.plan.target.tag(),
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
            "protocol": {
                "current": current_protocol(),
                "target": self.plan.vouched().map(|entry| &entry.protocol),
            },
            "artifact": self.artifact.as_ref().map(|artifact| serde_json::json!({
                "archive": artifact.name,
                "archive_url": artifact.url,
                "checksum_url": artifact.sidecar_url(),
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
            format!("current:  {}", self.current_label()),
            format!("latest:   {}", self.latest_label()),
            format!("channel:  {}", self.plan.channel.as_str()),
            format!(
                "source:   {} ({})",
                install.source.as_str(),
                install.executable.display()
            ),
        ];
        lines.extend(self.action_lines());
        if let Some(handoff) = self.handoff.as_ref() {
            lines.push(handoff_line(handoff));
        }
        lines
    }

    fn current_label(&self) -> String {
        channel::display(
            &self.plan.current.to_string(),
            channel::of_build(),
            self.plan.current_sha.as_deref(),
        )
    }

    fn latest_label(&self) -> String {
        match self.plan.channel {
            Channel::Stable => self.plan.latest_tag(),
            Channel::Next => channel::display(
                &self.plan.current.to_string(),
                Channel::Next,
                self.plan.latest_sha(),
            ),
        }
    }

    fn action_lines(&self) -> Vec<String> {
        let install = &self.plan.install;
        let target = self.plan.target.tag();
        match self.action {
            Action::Checked => checked_lines(install, &self.plan),
            Action::UpToDate => vec![format!("already on {target}")],
            Action::Planned => {
                let mut lines = Vec::new();
                if let Some(artifact) = self.artifact.as_ref() {
                    lines.push(format!("verified: {}", artifact.name));
                }
                if let Some(digest) = self.digest.as_ref() {
                    lines.push(format!("sha256:   {digest}"));
                }
                lines.push(format!(
                    "dry run: would install {target} and replace {}",
                    self.binaries.join(", ")
                ));
                lines
            }
            Action::Installed => installed_lines(self),
            Action::RolledBack => vec![format!("restored {target}: {}", self.binaries.join(", "))],
        }
    }
}

fn checked_lines(install: &Install, plan: &Plan) -> Vec<String> {
    let mut lines = vec![if plan.update_available {
        format!(
            "an update is available: {} -> {}",
            plan.current,
            plan.target.tag()
        )
    } else {
        "already on the latest release".to_owned()
    }];
    // The install source is always reported, so an unmaintainable install is
    // learned at check time, not at failure time.
    if let Some(refusal) = Refusal::of(install.source) {
        lines.push(refusal.message(install));
        lines.extend(
            Refusal::remedy(install)
                .lines()
                .map(|line| format!("  {line}")),
        );
    } else {
        if plan.update_available {
            lines.push("run `phux update` to install it".to_owned());
        }
        lines.push(switch_line());
    }
    lines
}

/// The check report names both rail switches (a legend, not a per-rail hint,
/// because `--check --channel next` can run on a stable install).
fn switch_line() -> String {
    "switch:   `phux channel next` follows green main; \
     `phux channel latest` returns to numbered releases"
        .to_owned()
}

fn installed_lines(outcome: &Outcome) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(digest) = outcome.digest.as_ref() {
        lines.push(format!("sha256:   {digest} (verified)"));
    }
    lines.push(format!(
        "installed {}: {}",
        outcome.plan.target.tag(),
        outcome.binaries.join(", ")
    ));
    if let Some(backup) = outcome.backup.as_ref() {
        lines.push(format!(
            "previous binaries saved in {} (`phux update --rollback` restores them)",
            backup.display()
        ));
    }
    lines
}

fn handoff_line(handoff: &Handoff) -> String {
    match handoff {
        Handoff::Upgrading => "server upgrading in place; sessions preserved".to_owned(),
        Handoff::NoServer => {
            "no server was running; the next `phux` starts the new binary".to_owned()
        }
        Handoff::Skipped => {
            "server left alone (--no-restart); run `phux upgrade` when ready".to_owned()
        }
        Handoff::Refused(message) => {
            let next = match super::upgrade::classify(message) {
                super::upgrade::RefusalClass::Transient => "run `phux upgrade` to retry",
                super::upgrade::RefusalClass::LegacyAgentSessions => {
                    "`phux upgrade --close-agent-sessions` closes its agent sessions and retries"
                }
                super::upgrade::RefusalClass::Fatal => "fix the cause, then run `phux upgrade`",
            };
            format!(
                "the running server refused the handoff: {message}\n  \
                 it keeps serving the old image; {next}"
            )
        }
        Handoff::Failed(message) => format!(
            "the handoff could not be delivered: {message}\n  \
             the new binary is installed; run `phux upgrade` to retry"
        ),
    }
}

/// Decide what to install: the manifest's build, or a `--version` pin, set
/// against what `installed` holds. Pure given the resolved inputs. A pin does
/// not depend on discovery: it is the way back to a known release when the
/// manifest is unreachable or wrong.
fn plan(
    install: Install,
    current: Version,
    installed: &Build,
    channel: Channel,
    published: Result<Entry, UpdateError>,
    pin: Option<Version>,
    host_target: &'static str,
) -> Result<Plan, UpdateError> {
    let (latest, target) = if let Some(version) = pin {
        (published.ok(), Build::Stable(version))
    } else {
        let latest = published?;
        let target = latest.build.clone();
        (Some(latest), target)
    };
    Ok(Plan {
        install,
        current,
        channel,
        current_sha: channel::build_sha().map(str::to_owned),
        latest,
        changes_version: *installed != target,
        update_available: shared::is_newer(installed, &target),
        target,
        host_target,
    })
}

/// Run one `phux update` against injected effects, so the whole flow is
/// testable with a fake [`ReleaseSource`] and a scratch bin directory.
pub(crate) fn execute(opts: &UpdateOpts, env: &UpdateEnv<'_>) -> Result<Outcome, UpdateError> {
    let install = env.install.clone();
    let current = release::current_version()?;
    if opts.rollback {
        return rollback(opts, install, current, env);
    }

    let host_target = release::host_target()?;
    let pin = opts.tag.as_deref().map(release::validate_tag).transpose()?;
    let channel = channel::resolve(opts.channel.map(Channel::from), &install);
    if channel == Channel::Next && pin.is_some() {
        return Err(UpdateError::InvalidTag(
            "`--version` pins a stable tag; omit it when using `--channel next`".to_owned(),
        ));
    }
    let installed = channel::installed(&install, current);
    let published = env
        .releases
        .manifest(channel)
        .and_then(|body| release::published(&body, channel, host_target));
    let plan = plan(
        install,
        current,
        &installed,
        channel,
        published,
        pin,
        host_target,
    )?;

    let action = if opts.check {
        Action::Checked
    } else if plan.changes_version {
        return install_release(opts, env, plan);
    } else {
        Action::UpToDate
    };
    Ok(Outcome {
        action,
        plan,
        artifact: None,
        digest: None,
        binaries: Vec::new(),
        backup: None,
        handoff: None,
    })
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
    let artifact = shared::asset(Product::Phux, &plan.target, plan.host_target);
    let staging = apply::Staging::create(&bin_dir)?;

    // Downloads land in staging, removed on every exit path.
    let archive_path = staging.path().join(&artifact.name);
    env.releases.download(&artifact.url, &archive_path)?;

    // The gate: nothing below runs unless the published digest matches.
    let expected = if let Some(entry) = plan.vouched() {
        entry.sha256.clone()
    } else {
        let sidecar_path = staging.path().join(format!("{}.sha256", artifact.name));
        env.releases
            .download(&artifact.sidecar_url(), &sidecar_path)?;
        let sidecar = std::fs::read_to_string(&sidecar_path).map_err(|err| {
            UpdateError::Checksum(format!("could not read the .sha256 sidecar: {err}"))
        })?;
        apply::expected_digest(&sidecar, &artifact.name)?
    };
    let digest = apply::verify_digest(&archive_path, &expected, &artifact.name)?;
    let staged = apply::unpack_verified(&archive_path, &artifact.stem, staging.path())?;

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
    channel::persist(&bin_dir, plan.channel)?;
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

/// Restore the binaries saved by the previous update. The handoff runs here
/// too, so the running server goes back to the previous image;
/// `--no-restart` suppresses it as on the way forward.
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
    let target = Version::parse(&restored.version).unwrap_or(current);
    let plan = Plan {
        install,
        current,
        channel: Channel::Stable,
        current_sha: None,
        latest: None,
        target: Build::Stable(target),
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
    let (socket_path, explicit_socket) = super::upgrade::resolve_socket(socket);

    // Refuse a mutation of an install phux does not own before any network
    // traffic; `--check` may always report.
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

    // Machine output never prompts and keeps stderr quiet; the flag still
    // grants the agent-session recovery.
    let consent = if opts.json && !opts.close_agent_sessions {
        super::upgrade::Consent::Advise
    } else {
        super::upgrade::Consent::detect(opts.close_agent_sessions)
    };
    let handoff = || live_handoff(&socket_path, explicit_socket.as_deref(), consent, opts.json);
    let env = UpdateEnv {
        releases: &release::NetworkReleaseSource,
        handoff: &handoff,
        install,
    };

    match execute(opts, &env) {
        Ok(outcome) => {
            let shadow = std::env::var_os("PATH")
                .and_then(|path| shadowing_phux(&outcome.plan.install, &path));
            if let Err(err) = print_outcome(&outcome, shadow.as_deref(), opts.json) {
                return json_err::emit(
                    true,
                    &CliError::new(
                        codes::JSON_SERIALIZE,
                        format!("could not render the update document: {err}"),
                        "report this at https://github.com/no-phux/phux/issues".to_owned(),
                    ),
                    EXIT_FAILURE,
                );
            }
            reconcile_installed(outcome.action, opts.json, |print| {
                super::service::reconcile_after_update(print);
            });
            ExitCode::from(EXIT_SUCCESS)
        }
        Err(err) => err.report(opts.json),
    }
}

/// Print the update result and any stale binary shadowing it on PATH.
fn print_outcome(
    outcome: &Outcome,
    shadow: Option<&std::path::Path>,
    json: bool,
) -> Result<(), serde_json::Error> {
    if json {
        let mut document = outcome.document();
        document["path_shadowed_by"] = shadow.map(|path| path.display().to_string()).into();
        outln!("{}", serde_json::to_string_pretty(&document)?);
        return Ok(());
    }
    for line in outcome.lines() {
        outln!("{line}");
    }
    if let Some(path) = shadow {
        outln!("{}", shadow_warning(path, &outcome.plan.install.executable));
    }
    Ok(())
}

/// The first `phux` on `PATH`, when it is not the binary this command
/// maintains. A stale `cargo install` or version-manager shim ahead of the
/// release install makes every update look like it did nothing.
fn shadowing_phux(install: &Install, path: &std::ffi::OsStr) -> Option<PathBuf> {
    if install.source != InstallSource::DirectRelease {
        return None;
    }
    let ours = std::fs::canonicalize(&install.executable).ok()?;
    let first = std::env::split_paths(path)
        .map(|dir| dir.join("phux"))
        .find(|candidate| crate::companion::is_executable(candidate))?;
    let resolved = std::fs::canonicalize(&first).ok()?;
    (resolved != ours).then_some(first)
}

fn shadow_warning(shadow: &std::path::Path, ours: &std::path::Path) -> String {
    format!(
        "warning:  `phux` on your PATH is {} first, not {}\n  \
         remove the stale copy or put {} earlier on PATH (`which -a phux` lists them)",
        shadow.display(),
        ours.display(),
        ours.parent().unwrap_or(ours).display()
    )
}

/// Reconcile every successful install while keeping machine output silent.
fn reconcile_installed(action: Action, json: bool, reconcile: impl FnOnce(bool)) {
    if action == Action::Installed {
        reconcile(!json);
    }
}

/// Ask the running server to re-exec, through `phux upgrade`'s own driver:
/// transient refusals are retried, and an older server blocked by agent
/// sessions gets them closed under `consent`. `socket` is the explicit
/// `--socket`, echoed into any printed command; `quiet` keeps stderr clean
/// for `--json`.
fn live_handoff(
    socket_path: &std::path::Path,
    socket: Option<&std::path::Path>,
    consent: super::upgrade::Consent,
    quiet: bool,
) -> Handoff {
    use phux_client::attach::AttachError;

    let outcome = super::upgrade::LiveUpgrade::new(socket_path, quiet)
        .and_then(|mut env| super::upgrade::drive(&mut env, consent, socket));
    match outcome {
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

    use phux_client_runtime::release::{self as shared, Asset, Build, Channel, Product, Version};

    use super::apply::BACKUP_DIR;
    use super::channel::{self, ChannelArg};
    use super::release::{ReleaseSource, current_version, host_target};
    use super::source::{Install, InstallSource};
    use super::{
        Action, Handoff, Outcome, Refusal, UpdateEnv, UpdateError, UpdateOpts, execute, plan,
        reconcile_installed,
    };

    const NEXT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn installed_updates_reconcile_in_human_and_json_modes() {
        let mut calls = Vec::new();
        reconcile_installed(Action::Installed, false, |print| calls.push(print));
        reconcile_installed(Action::Installed, true, |print| calls.push(print));
        reconcile_installed(Action::Checked, false, |print| calls.push(print));

        assert_eq!(calls, [true, false]);
    }

    /// The triple these tests publish artifacts under, resolved exactly as
    /// the code under test resolves it (a hardcoded triple 404s elsewhere).
    fn target() -> &'static str {
        host_target().expect("tests run on a platform with a published artifact")
    }

    const UNUSED_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    /// A channel manifest naming `version` and `sha` with `digest` for this
    /// host's tarball.
    fn manifest(channel: Channel, version: &str, sha: &str, digest: &str) -> String {
        serde_json::json!({
            "schema_version": 1,
            "channel": channel.as_str(),
            "generated_at": "2026-01-01T00:00:00Z",
            "products": {"phux": {
                "version": version,
                "sha": sha,
                "protocol": "0.9",
                "sha256": {target(): digest},
            }},
        })
        .to_string()
    }

    /// A [`ReleaseSource`] backed by a manifest per channel and a map of URL
    /// to bytes; no network.
    #[derive(Debug)]
    struct FakeReleases {
        manifests: HashMap<&'static str, String>,
        files: HashMap<String, Vec<u8>>,
        downloads: RefCell<Vec<String>>,
    }

    impl FakeReleases {
        /// A stable manifest naming `latest`, whose artifact is not served.
        fn new(latest: &str) -> Self {
            let mut fake = Self {
                manifests: HashMap::new(),
                files: HashMap::new(),
                downloads: RefCell::new(Vec::new()),
            };
            let version = latest.trim_start_matches('v');
            fake.name(Channel::Stable, version, &"c".repeat(40), UNUSED_DIGEST);
            fake
        }

        fn name(&mut self, channel: Channel, version: &str, sha: &str, digest: &str) {
            self.manifests
                .insert(channel.as_str(), manifest(channel, version, sha, digest));
        }

        fn serve(&mut self, url: &str, bytes: Vec<u8>) {
            self.files.insert(url.to_owned(), bytes);
        }
    }

    impl ReleaseSource for FakeReleases {
        fn manifest(&self, channel: Channel) -> Result<String, UpdateError> {
            self.manifests
                .get(channel.as_str())
                .cloned()
                .ok_or_else(|| UpdateError::Fetch(format!("no {} manifest", channel.as_str())))
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

    fn opts() -> UpdateOpts {
        UpdateOpts {
            check: false,
            dry_run: false,
            tag: None,
            channel: None,
            rollback: false,
            no_restart: true,
            close_agent_sessions: false,
            json: false,
        }
    }

    fn check() -> UpdateOpts {
        UpdateOpts {
            check: true,
            ..opts()
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

    fn direct(path: &Path) -> Install {
        install_at(path, InstallSource::DirectRelease)
    }

    /// Run `execute` against `fake` with a fixed handoff answer.
    fn run(
        fake: &FakeReleases,
        install: Install,
        handoff: Handoff,
        opts: &UpdateOpts,
    ) -> Result<Outcome, UpdateError> {
        let handoff = move || handoff.clone();
        let env = UpdateEnv {
            releases: fake,
            handoff: &handoff,
            install,
        };
        execute(opts, &env)
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// Build a release tarball plus its sidecar, serve both where the real
    /// naming puts them, and name it in the stable manifest.
    fn publish(fake: &mut FakeReleases, workdir: &Path, tag: &str) -> Asset {
        let version = tag.trim_start_matches('v');
        let (artifact, digest) =
            publish_artifact(fake, workdir, &Build::Stable(self::version(version)), tag);
        fake.name(Channel::Stable, version, &"c".repeat(40), &digest);
        artifact
    }

    fn publish_next(fake: &mut FakeReleases, workdir: &Path, sha: &str) -> Asset {
        let (artifact, digest) = publish_artifact(fake, workdir, &Build::Next(sha.to_owned()), sha);
        fake.name(Channel::Next, "9.9.9", sha, &digest);
        artifact
    }

    /// A packed release served with its sidecar; the manifest is untouched.
    fn publish_artifact(
        fake: &mut FakeReleases,
        workdir: &Path,
        build: &Build,
        label: &str,
    ) -> (Asset, String) {
        let artifact = shared::asset(Product::Phux, build, target());
        let build = workdir.join("build").join(&artifact.stem);
        fs::create_dir_all(&build).unwrap();
        for name in ["phux", "phux-mcp"] {
            fs::write(build.join(name), format!("{name} {label}")).unwrap();
            fs::set_permissions(build.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        for name in ["README.md", "LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"] {
            fs::write(build.join(name), b"text").unwrap();
        }

        let archive = workdir.join(&artifact.name);
        let status = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(workdir.join("build"))
            .arg(&artifact.stem)
            .status()
            .unwrap();
        assert!(status.success());

        let digest = super::apply::sha256_file(&archive).unwrap();
        fake.serve(&artifact.url, fs::read(&archive).unwrap());
        fake.serve(
            &artifact.sidecar_url(),
            format!("{digest}  {}\n", artifact.name).into_bytes(),
        );
        (artifact, digest)
    }

    /// The plan for an install of `installed` against a manifest naming
    /// `version` and `sha`, optionally pinned.
    fn plan_for(
        installed: &Build,
        channel: Channel,
        version: &str,
        sha: &str,
        pin: Option<&str>,
    ) -> Result<super::Plan, UpdateError> {
        let body = manifest(channel, version, sha, UNUSED_DIGEST);
        let published = super::release::published(&body, channel, target());
        let pin = pin.map(super::release::validate_tag).transpose()?;
        let current = match installed {
            Build::Stable(version) => *version,
            Build::Next(_) => self::version("0.13.0"),
        };
        plan(
            direct(Path::new("/home/ada/.local/bin/phux")),
            current,
            installed,
            channel,
            published,
            pin,
            target(),
        )
    }

    /// A bin directory holding a "current" install.
    fn seed_bin(scratch: &Path) -> PathBuf {
        let bin = scratch.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for name in ["phux", "phux-mcp"] {
            fs::write(bin.join(name), format!("old {name}")).unwrap();
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }

    #[test]
    fn a_stable_plan_classifies_updates_downgrades_and_bad_tags() {
        let installed = Build::Stable(version("0.12.1"));
        let plan = |pin| plan_for(&installed, Channel::Stable, "0.13.0", &"c".repeat(40), pin);
        let newer = plan(None).unwrap();
        assert!(newer.update_available && newer.changes_version);
        assert_eq!(newer.target.tag(), "v0.13.0");
        assert_eq!(
            newer.vouched().map(|entry| entry.sha256.as_str()),
            Some(UNUSED_DIGEST)
        );

        // An explicit older tag is a change, not an update, and the manifest
        // does not vouch for it.
        let older = plan(Some("v0.11.0")).unwrap();
        assert!(!older.update_available && older.changes_version);
        assert_eq!(older.target.tag(), "v0.11.0");
        assert!(older.vouched().is_none());

        let err = plan(Some("nightly")).unwrap_err();
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

    /// `--check` downloads nothing, names both rail switches for an install
    /// phux owns, and gives a package-managed install its native command
    /// instead of a switch.
    #[test]
    fn check_reports_without_downloading_anything() {
        let fake = FakeReleases::new("v0.13.0");
        let outcome = run(
            &fake,
            direct(Path::new("/home/ada/.local/bin/phux")),
            Handoff::Skipped,
            &check(),
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Checked);
        assert_eq!(outcome.plan.latest_tag(), "v0.13.0");
        assert!(outcome.artifact.is_none());
        assert!(
            fake.downloads.borrow().is_empty(),
            "--check must not download"
        );

        let doc = outcome.document();
        assert_eq!(doc["schema_version"], 2);
        assert_eq!(doc["action"], "checked");
        assert_eq!(doc["install"]["source"], "direct-release");
        assert_eq!(doc["install"]["mutable"], true);
        assert_eq!(doc["latest_version"], "v0.13.0");
        assert_eq!(doc["artifact"], serde_json::Value::Null);
        assert_eq!(doc["server_handoff"], serde_json::Value::Null);

        let switch = outcome
            .lines()
            .into_iter()
            .find(|line| line.starts_with("switch:"))
            .expect("the check report must name the rail switches");
        assert!(switch.contains("phux channel next"), "{switch}");
        assert!(switch.contains("phux channel latest"), "{switch}");

        let brew = run(
            &fake,
            install_at(
                Path::new("/opt/homebrew/Cellar/phux/0.12.1/bin/phux"),
                InstallSource::Homebrew,
            ),
            Handoff::Skipped,
            &check(),
        )
        .unwrap();
        let doc = brew.document();
        assert_eq!(doc["install"]["mutable"], false);
        assert_eq!(
            doc["install"]["native_command"],
            "brew upgrade no-phux/tap/phux"
        );
        let lines = brew.lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("brew upgrade no-phux/tap/phux")),
            "{lines:?}"
        );
        assert!(
            lines.iter().all(|line| !line.starts_with("switch:")),
            "a refused install must not be told to switch: {lines:?}"
        );
    }

    #[test]
    fn dry_run_downloads_and_verifies_but_installs_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v9.9.9");
        publish(&mut fake, scratch.path(), "v9.9.9");

        let outcome = run(
            &fake,
            direct(&bin.join("phux")),
            Handoff::Skipped,
            &UpdateOpts {
                dry_run: true,
                ..opts()
            },
        )
        .unwrap();

        assert_eq!(outcome.action, Action::Planned);
        assert!(outcome.digest.is_some(), "the checksum must be verified");
        assert_eq!(outcome.binaries, vec!["phux-mcp", "phux"]);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert!(!bin.join(BACKUP_DIR).exists());
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
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v9.9.9");
        let artifact = publish(&mut fake, scratch.path(), "v9.9.9");

        let outcome = run(
            &fake,
            direct(&bin.join("phux")),
            Handoff::Upgrading,
            &UpdateOpts {
                no_restart: false,
                ..opts()
            },
        )
        .unwrap();

        assert_eq!(outcome.action, Action::Installed);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");
        assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"phux-mcp v9.9.9");
        assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

        let doc = outcome.document();
        assert_eq!(doc["action"], "installed");
        assert_eq!(doc["target_version"], "v9.9.9");
        assert_eq!(doc["artifact"]["archive"], artifact.name);
        assert_eq!(doc["server_handoff"]["result"], "upgrading");
        assert!(doc["artifact"]["sha256"].is_string());
        assert!(doc["backup"].is_string());
        assert_eq!(doc["protocol"]["target"], "0.9");
        assert_eq!(doc["protocol"]["current"], super::current_protocol());

        // The manifest's digest is the gate; the sidecar is not fetched.
        let fetched = fake.downloads.borrow().clone();
        assert_eq!(fetched, vec![artifact.url]);
    }

    /// A pin to a release the manifest does not name is verified against that
    /// release's own sidecar, and still works when the manifest is gone.
    #[test]
    fn a_pin_verifies_its_sidecar_and_survives_a_missing_manifest() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v9.9.9");
        let (pinned, _) = publish_artifact(
            &mut fake,
            scratch.path(),
            &Build::Stable(version("9.9.8")),
            "v9.9.8",
        );
        let pin = UpdateOpts {
            tag: Some("v9.9.8".to_owned()),
            ..opts()
        };

        let outcome = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &pin).unwrap();
        assert_eq!(outcome.action, Action::Installed);
        assert_eq!(outcome.plan.latest_tag(), "v9.9.9");
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.8");
        assert!(fake.downloads.borrow().contains(&pinned.sidecar_url()));

        fake.manifests.clear();
        let outcome = run(
            &fake,
            direct(&bin.join("phux")),
            Handoff::Skipped,
            &UpdateOpts {
                dry_run: true,
                ..pin
            },
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Planned);
        assert_eq!(outcome.plan.latest_tag(), "v9.9.8");

        let err = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
        assert!(matches!(err, UpdateError::Fetch(_)), "{err:?}");
    }

    /// A tampered archive and a missing artifact both fail without touching
    /// the install.
    #[test]
    fn a_tampered_or_missing_archive_is_refused_and_nothing_is_replaced() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v9.9.9");
        let artifact = publish(&mut fake, scratch.path(), "v9.9.9");
        // Swap the archive after the sidecar was published.
        fake.serve(&artifact.url, b"totally different bytes".to_vec());

        let err = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
        assert!(
            matches!(err, UpdateError::ChecksumMismatch { .. }),
            "{err:?}"
        );
        assert_eq!(err.exit_code(), 1);
        assert_eq!(err.code(), "update_checksum_mismatch");
        assert!(err.to_string().contains("checksum mismatch"));
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert!(!bin.join(BACKUP_DIR).exists());

        let empty = FakeReleases::new("v9.9.9");
        let err = run(&empty, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap_err();
        assert!(matches!(err, UpdateError::Fetch(_)), "{err:?}");
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    }

    #[test]
    fn an_install_already_on_the_target_release_does_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let fake = FakeReleases::new(&format!("v{}", current_version().unwrap()));

        let outcome = run(&fake, direct(&bin.join("phux")), Handoff::Skipped, &opts()).unwrap();
        assert_eq!(outcome.action, Action::UpToDate);
        assert!(!outcome.plan.update_available);
        assert!(fake.downloads.borrow().is_empty());
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
    }

    #[test]
    fn rollback_restores_the_previous_release_and_hands_off_again() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v9.9.9");
        publish(&mut fake, scratch.path(), "v9.9.9");
        let install = direct(&bin.join("phux"));
        let rollback = UpdateOpts {
            rollback: true,
            ..opts()
        };

        run(&fake, install.clone(), Handoff::Upgrading, &opts()).unwrap();
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"phux v9.9.9");

        let outcome = run(&fake, install.clone(), Handoff::Upgrading, &rollback).unwrap();
        assert_eq!(outcome.action, Action::RolledBack);
        assert_eq!(fs::read(bin.join("phux")).unwrap(), b"old phux");
        assert_eq!(fs::read(bin.join("phux-mcp")).unwrap(), b"old phux-mcp");
        assert_eq!(outcome.document()["action"], "rolled-back");
        // `--no-restart` leaves the server alone on the way back too.
        assert_eq!(outcome.handoff, Some(Handoff::Skipped));
        assert_eq!(outcome.document()["server_handoff"]["result"], "skipped");

        // Without it, the rollback hands the live server back.
        run(&fake, install.clone(), Handoff::Upgrading, &opts()).unwrap();
        let outcome = run(
            &fake,
            install.clone(),
            Handoff::Upgrading,
            &UpdateOpts {
                no_restart: false,
                ..rollback
            },
        )
        .unwrap();
        assert_eq!(outcome.handoff, Some(Handoff::Upgrading));

        // Nothing left to restore.
        let err = run(
            &fake,
            install,
            Handoff::Upgrading,
            &UpdateOpts {
                rollback: true,
                ..opts()
            },
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::NoBackup(_)), "{err:?}");
        assert_eq!(err.code(), "update_no_backup");
    }

    #[test]
    fn next_channel_and_a_stable_tag_are_refused_together() {
        let err = run(
            &FakeReleases::new("v0.13.0"),
            direct(Path::new("/home/ada/.local/bin/phux")),
            Handoff::Skipped,
            &UpdateOpts {
                channel: Some(ChannelArg::Next),
                tag: Some("v0.13.0".to_owned()),
                ..check()
            },
        )
        .unwrap_err();
        assert!(matches!(err, UpdateError::InvalidTag(_)), "{err:?}");
    }

    #[test]
    fn check_on_next_does_not_download_an_archive() {
        let mut fake = FakeReleases::new("v0.13.0");
        fake.name(Channel::Next, "0.13.0", NEXT_SHA, UNUSED_DIGEST);
        let outcome = run(
            &fake,
            direct(Path::new("/home/ada/.local/bin/phux")),
            Handoff::Skipped,
            &UpdateOpts {
                channel: Some(ChannelArg::Next),
                ..check()
            },
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Checked);
        assert_eq!(outcome.plan.channel, Channel::Next);
        assert_eq!(outcome.plan.latest_sha(), Some(NEXT_SHA));
        assert!(outcome.plan.update_available);
        assert!(fake.downloads.borrow().is_empty());
        assert_eq!(outcome.document()["channel"], "next");
        assert_eq!(outcome.document()["latest_sha"], NEXT_SHA);
    }

    #[test]
    fn a_next_update_installs_and_persists_the_channel() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = seed_bin(scratch.path());
        let mut fake = FakeReleases::new("v0.13.0");
        let artifact = publish_next(&mut fake, scratch.path(), NEXT_SHA);
        let outcome = run(
            &fake,
            direct(&bin.join("phux")),
            Handoff::Skipped,
            &UpdateOpts {
                channel: Some(ChannelArg::Next),
                ..opts()
            },
        )
        .unwrap();
        assert_eq!(outcome.action, Action::Installed);
        assert_eq!(
            fs::read(bin.join("phux")).unwrap(),
            format!("phux {NEXT_SHA}").as_bytes()
        );
        assert_eq!(channel::read_file(&bin), Some(Channel::Next));
        let fetched = fake.downloads.borrow().clone();
        assert_eq!(fetched, vec![artifact.url]);
    }

    #[test]
    fn a_next_plan_is_up_to_date_only_on_the_same_sha() {
        let plan = |sha: &str| {
            plan_for(
                &Build::Next(sha.to_owned()),
                Channel::Next,
                "0.13.0",
                NEXT_SHA,
                None,
            )
            .unwrap()
        };
        let current = plan(NEXT_SHA);
        assert!(!current.update_available && !current.changes_version);
        assert!(plan(&"a".repeat(40)).update_available);
        assert!(
            plan_for(
                &Build::Stable(version("0.13.0")),
                Channel::Next,
                "0.13.0",
                NEXT_SHA,
                None
            )
            .unwrap()
            .update_available
        );
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

    #[test]
    fn a_stale_phux_earlier_on_path_is_named() {
        let temp = tempfile::tempdir().unwrap();
        let stale = temp.path().join("cargo-bin");
        let ours = temp.path().join("local-bin");
        for dir in [&stale, &ours] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("phux"), "#!/bin/sh\n").unwrap();
            fs::set_permissions(dir.join("phux"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        let install = direct(&ours.join("phux"));
        let path = std::env::join_paths([&stale, &ours]).unwrap();
        assert_eq!(
            super::shadowing_phux(&install, &path),
            Some(stale.join("phux"))
        );
        let path = std::env::join_paths([&ours, &stale]).unwrap();
        assert_eq!(super::shadowing_phux(&install, &path), None);
        let brew = install_at(&ours.join("phux"), InstallSource::Homebrew);
        let path = std::env::join_paths([&stale, &ours]).unwrap();
        assert_eq!(super::shadowing_phux(&brew, &path), None);
    }

    /// The desktop links the protocol in-process, and a server refuses a
    /// client whose protocol `major.minor` differs. Its release train only
    /// cuts a release for commits under its own directory, so a protocol bump
    /// must edit the pin in the same commit to release the matching app.
    #[test]
    fn desktop_release_train_pins_the_protocol_it_links() {
        let protocol = format!(
            "{}.{}",
            phux_protocol::PROTOCOL_VERSION.major,
            phux_protocol::PROTOCOL_VERSION.minor
        );
        let pin = include_str!("../../../../../clients/desktop/phux-protocol");
        assert_eq!(
            pin.trim(),
            protocol,
            "protocol is {protocol}: set clients/desktop/phux-protocol to {protocol} in this \
             commit so the app that links it is released with it"
        );
    }
}
