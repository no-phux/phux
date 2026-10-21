//! The update error, options, and plan.

use super::{
    Args, Build, Channel, ChannelArg, CliError, EXIT_FAILURE, EXIT_USAGE, Entry, ExitCode, Install,
    Version, codes, json_err,
};

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
    /// The checksum's signature was missing, malformed, made by a key this
    /// binary does not embed, or did not verify; or the signed digest and
    /// the channel manifest disagree.
    Signature(String),
    /// The channel manifest names a stable release older than the installed
    /// one and no `--version` pin asked for it. A rewritten manifest must not
    /// roll a user back to a genuinely signed, older release (ADR-0178).
    Downgrade(String),
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
            | Self::Signature(message)
            | Self::Downgrade(message)
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
    pub(super) const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTag(_) => codes::UPDATE_INVALID_TAG,
            Self::UnsupportedPlatform(_) => codes::UPDATE_UNSUPPORTED_PLATFORM,
            Self::Fetch(_) => codes::UPDATE_FETCH_FAILED,
            Self::Checksum(_) => codes::UPDATE_CHECKSUM_INVALID,
            Self::Signature(_) => codes::UPDATE_SIGNATURE_INVALID,
            Self::Downgrade(_) => codes::UPDATE_DOWNGRADE_REFUSED,
            Self::ChecksumMismatch { .. } => codes::UPDATE_CHECKSUM_MISMATCH,
            Self::Archive(_) => codes::UPDATE_ARCHIVE_REJECTED,
            Self::Install(_) => codes::UPDATE_INSTALL_FAILED,
            Self::NoBackup(_) => codes::UPDATE_NO_BACKUP,
        }
    }

    /// Usage-class failures exit 2; everything else exits 1.
    pub(super) const fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidTag(_) | Self::UnsupportedPlatform(_) => EXIT_USAGE,
            _ => EXIT_FAILURE,
        }
    }

    /// The way out, in the caller's own terms.
    pub(super) fn remedy(&self) -> String {
        match self {
            Self::InvalidTag(_) => {
                "pass a tag from https://github.com/no-phux/phux/releases, like `--version v1.2.3`"
                    .to_owned()
            }
            Self::UnsupportedPlatform(_) => {
                "build from source: see https://docs.phux.sh/installation#build-from-source".to_owned()
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
            Self::Signature(_) => "nothing was installed. Re-run in a minute in case a \
                 release is still uploading; if it fails again, do not install this \
                 artifact by hand — report it at https://github.com/no-phux/phux/issues"
                .to_owned(),
            Self::Downgrade(_) => "nothing was installed. To go back on purpose, pin the \
                 release: `phux update --version <tag>`"
                .to_owned(),
            Self::Archive(_) => "nothing was installed. Re-run to download again; a repeat \
                 failure means the published artifact is malformed"
                .to_owned(),
            Self::Install(_) => "the previous binaries are saved; `phux update --rollback` \
                 restores them"
                .to_owned(),
            Self::NoBackup(_) => "a backup exists only after a successful `phux update`; \
                 reinstall with the curl installer in docs/install.md"
                .to_owned(),
        }
    }

    /// Report on the right channel and return the process status.
    pub(super) fn report(&self, json: bool) -> ExitCode {
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
    pub(super) fn vouched(&self) -> Option<&Entry> {
        self.latest
            .as_ref()
            .filter(|latest| latest.build == self.target)
    }

    /// The newest release on this channel, as a tag.
    pub(super) fn latest_tag(&self) -> String {
        self.latest
            .as_ref()
            .map_or(&self.target, |latest| &latest.build)
            .tag()
    }

    /// The commit at the head of next, when following it.
    pub(super) fn latest_sha(&self) -> Option<&str> {
        self.latest
            .as_ref()
            .filter(|_| self.channel == Channel::Next)
            .map(|latest| latest.sha.as_str())
    }
}

/// The protocol `major.minor` this build speaks.
pub(super) fn current_protocol() -> String {
    format!(
        "{}.{}",
        phux_protocol::PROTOCOL_VERSION.major,
        phux_protocol::PROTOCOL_VERSION.minor
    )
}
