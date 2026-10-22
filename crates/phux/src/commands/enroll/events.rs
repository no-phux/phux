//! What an enrollment narrates and how it fails: supervision, the service
//! policy, progress events, and the typed failure.

/// Whether the remote server is kept alive by something, after
/// [`ensure_remote_server`] ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Supervision {
    /// The service unit was written and loaded: the server is running under
    /// launchd or systemd and comes back after a reboot.
    Service,
    /// A server was already live, so the unit was written and armed; that
    /// server keeps its panes and supervision takes over its next start.
    Adopted,
    /// No service manager would take the unit; `phux server --ensure` is
    /// running an unsupervised server instead. `reason` is the install
    /// failure.
    Unsupervised { reason: String },
    /// `--no-service`: only `phux server --ensure` ran.
    Skipped,
}

impl Supervision {
    pub(crate) const fn severity(&self) -> crate::output::Severity {
        match self {
            Self::Unsupervised { .. } => crate::output::Severity::Warn,
            Self::Service | Self::Adopted | Self::Skipped => crate::output::Severity::Info,
        }
    }

    /// One clause for a progress line.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Service => "server running, supervised by its service unit".to_owned(),
            Self::Adopted => {
                "server already running; its service unit is armed for the next start".to_owned()
            }
            Self::Unsupervised { reason } => format!(
                "server running unsupervised (service install failed: {reason}); it will not come back by itself after a reboot"
            ),
            Self::Skipped => {
                "server running unsupervised (--no-service); it will not come back by itself after a reboot".to_owned()
            }
        }
    }
}

/// Whether [`ensure_remote_server`] installs the host's service unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServicePolicy {
    /// Install (or adopt into) the per-user unit so the server survives
    /// reboot. The default.
    Install,
    /// `--no-service`: start a server now and nothing more.
    Skip,
}

/// A progress event from the shared ssh middle, rendered by the caller with
/// [`EnrollEvent::describe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnrollEvent {
    /// `phux --version` answered on the host.
    PhuxFound { version: String },
    /// The service question is settled one way or another.
    ServerReady(Supervision),
    /// Neither the service install nor `phux server --ensure` succeeded.
    /// Pairing is still tried, since a server may be live regardless, but
    /// `phux pair` mints nothing without a bound listener (ADR-0141).
    ServerStartFailed { error: String },
    /// The pairing document came back.
    Paired,
    /// A previously enrolled credential still works after the server was
    /// brought up; no new credential was minted.
    CredentialReused,
    /// A previously enrolled credential was revoked while minting its
    /// replacement.
    CredentialReplaced,
    /// A direct route is being dialed.
    Probing { endpoint: String },
    /// A direct route answered.
    DirectReachable { endpoint: String },
    /// A direct route did not answer within the deadline.
    DirectUnreachable { endpoint: String, reason: String },
    /// A workload client certificate was enrolled. Any credential it
    /// supersedes is still live until the entry names the new one.
    CertificateEnrolled { credential_id: String },
    /// No workload client certificate was enrolled. Not fatal: the host is
    /// paired, and dials fall back to the previous identity or none.
    CertificateNotEnrolled { reason: String },
    /// The held certificate is inside the renewal window (or unreadable), so
    /// it is replaced rather than kept.
    CertificateRenewalDue { detail: String },
    /// A credential was revoked on the far host: the one the entry named
    /// before, or a new one nothing here recorded.
    CertificateRevoked {
        credential_id: String,
        retired: Retired,
    },
    /// That revocation did not happen; `remedy` is the command that does it.
    CertificateNotRevoked {
        credential_id: String,
        retired: Retired,
        reason: String,
        remedy: String,
    },
    /// The far host holds no credential with the superseded id: the entry
    /// may have pointed at another host, where it stays admitted.
    CertificateNotHeld { credential_id: String },
    /// The entry named a client key without its certificate (or a
    /// certificate that does not read), so the far-host credential it
    /// supersedes cannot be named and is not revoked.
    PreviousCertificateUnidentified { remedy: String },
}

/// Why a credential is revoked on the far host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Retired {
    /// The entry named it before; the new pair superseded it.
    Superseded,
    /// It was enrolled, but no entry here records it, so no one holds its
    /// key.
    Unrecorded,
}

impl Retired {
    pub(super) const fn describe(self) -> &'static str {
        match self {
            Self::Superseded => "the superseded client certificate",
            Self::Unrecorded => "the client certificate nothing here recorded",
        }
    }
}

impl EnrollEvent {
    /// Severity comes from the event, not its rendered progress text.
    pub(crate) const fn severity(&self) -> crate::output::Severity {
        use crate::output::Severity;
        match self {
            Self::ServerReady(supervision) => supervision.severity(),
            Self::ServerStartFailed { .. }
            | Self::DirectUnreachable { .. }
            | Self::CertificateNotEnrolled { .. }
            | Self::CertificateRenewalDue { .. }
            | Self::CertificateNotRevoked { .. }
            | Self::CertificateNotHeld { .. }
            | Self::PreviousCertificateUnidentified { .. } => Severity::Warn,
            Self::Paired
            | Self::CredentialReused
            | Self::CredentialReplaced
            | Self::DirectReachable { .. }
            | Self::CertificateEnrolled { .. }
            | Self::CertificateRevoked { .. } => Severity::Ok,
            Self::PhuxFound { .. } | Self::Probing { .. } => Severity::Info,
        }
    }

    pub(crate) fn report_progress(&self, name: &str) {
        crate::output::diagnostic(self.severity(), format_args!("{name}: {}", self.describe()));
    }

    /// Whether this event leaves something for the operator to do about the
    /// client certificate: reported in `enrollment.warnings` under `--json`.
    pub(crate) const fn is_warning(&self) -> bool {
        matches!(
            self,
            Self::CertificateNotRevoked { .. }
                | Self::CertificateNotHeld { .. }
                | Self::PreviousCertificateUnidentified { .. }
        )
    }

    /// The progress line for this event, without a prefix: callers add the
    /// host name or `phux:` as their contract wants.
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::PhuxFound { version } => format!("{version} found over ssh"),
            Self::ServerReady(supervision) => supervision.describe(),
            Self::ServerStartFailed { error } => format!(
                "could not start a server there ({error}); pairing needs one with a remote listener, trying anyway"
            ),
            Self::Paired => "paired".to_owned(),
            Self::CredentialReused => {
                "previous credential still works; kept it (no new mint)".to_owned()
            }
            Self::CredentialReplaced => {
                "minted a new credential and revoked the previous one".to_owned()
            }
            Self::Probing { endpoint } => format!("trying {endpoint}"),
            Self::DirectReachable { endpoint } => format!("direct route reachable at {endpoint}"),
            Self::DirectUnreachable { endpoint, reason } => {
                format!("{endpoint} did not answer ({reason})")
            }
            Self::CertificateEnrolled { credential_id } => {
                format!("enrolled workload client certificate {credential_id}")
            }
            Self::CertificateNotEnrolled { reason } => format!(
                "no workload client certificate enrolled ({reason}); dials keep the previous one, or present none, until `phux host add` runs again"
            ),
            Self::CertificateRenewalDue { detail } => {
                format!("the workload client certificate {detail}; enrolling a new one")
            }
            Self::CertificateRevoked {
                credential_id,
                retired,
            } => format!("revoked {} {credential_id}", retired.describe()),
            Self::CertificateNotRevoked {
                credential_id,
                retired,
                reason,
                remedy,
            } => format!(
                "could not revoke {} {credential_id} ({reason}); it stays admitted until it expires or you run `{remedy}`",
                retired.describe()
            ),
            Self::CertificateNotHeld { credential_id } => format!(
                "this host holds no credential {credential_id}, the certificate the entry named before; if the entry used to reach another host, revoke it there with `phux workload revoke {credential_id}`"
            ),
            Self::PreviousCertificateUnidentified { remedy } => format!(
                "the entry named a client key without a readable certificate, so the credential it replaces cannot be named or revoked; find it with `{remedy}` and revoke it there"
            ),
        }
    }
}

/// Why the ssh middle stopped: one variant per remedy the callers phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnrollFailure {
    /// ssh itself failed (exit 255): resolution, connection, authentication.
    SshUnreachable(String),
    /// `phux` is not on the remote `PATH` (exit 127, or `phux --version`
    /// failed some other way).
    MissingPhux(String),
    /// Pairing failed: `phux pair --json` did not run, or its output did
    /// not parse.
    Pair(String),
}

impl EnrollFailure {
    /// The failure's own words, for callers that print them inline.
    pub(crate) fn detail(&self) -> &str {
        match self {
            Self::SshUnreachable(detail) | Self::MissingPhux(detail) | Self::Pair(detail) => detail,
        }
    }
}
