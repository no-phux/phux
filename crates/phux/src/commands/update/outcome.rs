//! Refusals, effects, handoff, and how an update reports what it did.

use super::{
    Asset, Build, Channel, DOCUMENT_SCHEMA_VERSION, Entry, Install, InstallSource, PathBuf, Plan,
    ReleaseSource, UnknownReason, UpdateError, Version, channel, codes, current_protocol, shared,
};

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
    pub(super) const fn of(source: InstallSource) -> Option<Self> {
        match source {
            InstallSource::DirectRelease => None,
            InstallSource::Nix => Some(Self::ImmutableStore),
            InstallSource::Homebrew | InstallSource::Cargo => Some(Self::PackageManaged),
            InstallSource::Unknown => Some(Self::UnknownSource),
        }
    }

    /// The stable `--json` error code.
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::ImmutableStore => codes::UPDATE_IMMUTABLE_STORE,
            Self::PackageManaged => codes::UPDATE_PACKAGE_MANAGED,
            Self::UnknownSource => codes::UPDATE_SOURCE_UNSUPPORTED,
        }
    }

    /// The refusal message for `install`.
    pub(super) fn message(self, install: &Install) -> String {
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
    pub(super) fn remedy(install: &Install) -> String {
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
                     docs/install.md, or set PHUX_INSTALL_DIR to this directory.",
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
    /// The minisign public keys a release checksum must be signed by:
    /// [`signature::RELEASE_KEYS`] outside tests.
    pub(crate) release_keys: &'a [&'a str],
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
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Upgrading => "upgrading",
            Self::NoServer => "no_server",
            Self::Refused(_) => "refused",
            Self::Skipped => "skipped",
            Self::Failed(_) => "failed",
        }
    }

    /// The human detail, when there is one.
    pub(super) fn detail(&self) -> Option<&str> {
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
    pub(super) const fn as_str(self) -> &'static str {
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
                "signature_url": artifact.signature_url(),
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

    pub(super) fn current_label(&self) -> String {
        channel::display(
            &self.plan.current.to_string(),
            channel::of_build(),
            self.plan.current_sha.as_deref(),
        )
    }

    pub(super) fn latest_label(&self) -> String {
        match self.plan.channel {
            Channel::Stable => self.plan.latest_tag(),
            Channel::Next => channel::display(
                &self.plan.current.to_string(),
                Channel::Next,
                self.plan.latest_sha(),
            ),
        }
    }

    pub(super) fn action_lines(&self) -> Vec<String> {
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

pub(super) fn checked_lines(install: &Install, plan: &Plan) -> Vec<String> {
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
pub(super) fn switch_line() -> String {
    "switch:   `phux channel next` follows green main; \
     `phux channel latest` returns to numbered releases"
        .to_owned()
}

pub(super) fn installed_lines(outcome: &Outcome) -> Vec<String> {
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

pub(super) fn handoff_line(handoff: &Handoff) -> String {
    match handoff {
        Handoff::Upgrading => "server upgrading in place; sessions preserved".to_owned(),
        Handoff::NoServer => {
            "no server was running; the next `phux` starts the new binary".to_owned()
        }
        Handoff::Skipped => {
            "server left alone (--no-restart); run `phux upgrade` when ready".to_owned()
        }
        Handoff::Refused(message) => {
            let next = match super::super::upgrade::classify(message) {
                super::super::upgrade::RefusalClass::Transient => "run `phux upgrade` to retry",
                super::super::upgrade::RefusalClass::LegacyAgentSessions => {
                    "`phux upgrade --close-agent-sessions` closes its agent sessions and retries"
                }
                super::super::upgrade::RefusalClass::Fatal => {
                    "fix the cause, then run `phux upgrade`"
                }
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
pub(super) fn plan(
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
