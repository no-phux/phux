//! The workload client identity on disk: the request, the per-credential
//! file pair, the held certificate, and when it is due for renewal.

use std::path::{Path, PathBuf};

use phux_dial::TlsClientIdentity;

use super::events::ServicePolicy;

/// Everything the shared middle needs to enroll one host.
pub(crate) struct EnrollRequest<'a> {
    /// The ssh destination, exactly as typed after `ssh`.
    pub(crate) ssh_host: &'a str,
    /// The `phux` to run on the host (`--remote-phux`).
    pub(crate) remote_phux: &'a str,
    /// `--endpoint`: the one address to register, probed but never
    /// second-guessed.
    pub(crate) endpoint_override: Option<&'a str>,
    /// The QUIC port to configure on the host and dial.
    pub(crate) quic_port: u16,
    /// Whether to install the host's service unit.
    pub(crate) service: ServicePolicy,
    /// Bearer token already held for this name when re-enrolling: probed first
    /// and reused if a direct route answers, else revoked via `--replace-token`.
    pub(crate) previous_token: Option<&'a str>,
    /// Certificate fingerprint saved with [`Self::previous_token`], required
    /// to probe a previous credential (ADR-0031 refuses an unpinned dial).
    pub(crate) previous_fingerprint: Option<&'a str>,
    /// The workload client certificate already held for this name, presented
    /// on the reuse probe and kept when the previous credential still works.
    pub(crate) previous_identity: Option<&'a ClientIdentityFiles>,
    /// Enroll a workload client certificate over the same ssh trust
    /// (ADR-0116); `None` for a role that presents none (a satellite).
    pub(crate) workload: Option<WorkloadEnrollment<'a>>,
}

/// Where and as what a workload client certificate is enrolled.
pub(crate) struct WorkloadEnrollment<'a> {
    /// The directory the key and certificate land in (`<state>/remotes`).
    pub(crate) dir: &'a Path,
    /// The registry name, which prefixes the two file names.
    pub(crate) name: &'a str,
}

/// A stored workload client identity: its two owner-only files and the
/// credential id the far host recorded for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientIdentityFiles {
    /// The certificate chain (PEM, leaf then CA).
    pub(crate) certificate: PathBuf,
    /// The private key (PEM). Named here, read only by the TLS stack.
    pub(crate) private_key: PathBuf,
    /// `sha256:` of the key: the far host's registry id for it.
    pub(crate) credential_id: Option<String>,
    /// Whether this enrollment minted it (and so owns its cleanup).
    pub(crate) fresh: bool,
}

impl ClientIdentityFiles {
    /// An identity already on disk, as a registry entry names it.
    pub(crate) fn existing(certificate: &Path, private_key: &Path) -> Self {
        Self {
            credential_id: phux_server::workload::stored_credential_id(certificate),
            certificate: certificate.to_path_buf(),
            private_key: private_key.to_path_buf(),
            fresh: false,
        }
    }

    /// The identity a dial presents.
    pub(crate) fn tls(&self) -> TlsClientIdentity {
        TlsClientIdentity::PemFiles {
            certificate: self.certificate.clone(),
            private_key: self.private_key.clone(),
        }
    }

    /// Whether these are a pair `phux host add` enrolled for `name` in
    /// `dir` (see [`is_enrolled_file`]).
    #[cfg(test)]
    pub(crate) fn enrolled_under(&self, dir: &Path, name: &str) -> bool {
        is_enrolled_file(&self.certificate, dir, name, CERT_EXTENSION)
            && is_enrolled_file(&self.private_key, dir, name, KEY_EXTENSION)
    }

    /// Remove the two files, when this enrollment minted them and nothing
    /// will record them.
    pub(crate) fn discard_if_fresh(&self) {
        if self.fresh {
            phux_server::workload::remove_identity_files(&self.private_key, &self.certificate);
        }
    }

    /// Why this identity should be replaced now, or `None` while it is good
    /// for more than [`RENEW_WITHIN_SECONDS`].
    pub(crate) fn renewal_due(&self, now: i64) -> Option<String> {
        renewal_due(&self.certificate, now)
    }
}

/// The extension of an enrolled certificate chain file.
pub(super) const CERT_EXTENSION: &str = ".pem";

/// The extension of an enrolled private key file.
pub(super) const KEY_EXTENSION: &str = ".key";

/// Whether `path` is a file `phux host add` enrolled for `name` in `dir`:
/// directly in `dir` and named `<name>.client.<hex digest><extension>`, as
/// [`identity_stem`] names them. Only such files are removed when a
/// re-enrollment supersedes them; a `client-cert` or `client-key` the
/// operator pointed at their own files is left alone.
pub(super) fn is_enrolled_file(path: &Path, dir: &Path, name: &str, extension: &str) -> bool {
    let prefix = format!("{name}.client.");
    path.parent() == Some(dir)
        && path
            .file_name()
            .and_then(|file| file.to_str())
            .and_then(|file| file.strip_prefix(&prefix))
            .and_then(|rest| rest.strip_suffix(extension))
            .is_some_and(|digest| {
                !digest.is_empty() && digest.bytes().all(|b| b.is_ascii_hexdigit())
            })
}

/// How long before a client certificate stops admitting `phux host add`
/// replaces it, dials warn, and `phux doctor` reports it: 14 days.
pub(crate) const RENEW_WITHIN_SECONDS: i64 = 14 * SECONDS_PER_DAY;

pub(super) const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// When the far host stops admitting the client certificate at `cert`, as
/// Unix seconds. The registry expiry is authoritative (workload-auth §7)
/// and issuance rounds the certificate's `notAfter` up to the next midnight
/// after it, so admission ends within the day before `notAfter`; this
/// answers the start of that day. `None` when the file does not read as a
/// certificate.
pub(crate) fn admission_ends(cert: &Path) -> Option<i64> {
    phux_server::workload::stored_certificate_expiry(cert)
        .map(|not_after| not_after.saturating_sub(SECONDS_PER_DAY))
}

/// Why the client certificate at `cert` should be renewed at `now`
/// (unreadable, expired, or within [`RENEW_WITHIN_SECONDS`]), in words that
/// follow "the workload client certificate"; `None` while it is good.
pub(crate) fn renewal_due(cert: &Path, now: i64) -> Option<String> {
    let Some(ends) = admission_ends(cert) else {
        return Some(format!("at {} cannot be read", cert.display()));
    };
    let remaining = ends.saturating_sub(now);
    if remaining > RENEW_WITHIN_SECONDS {
        return None;
    }
    let date = calendar_date(ends);
    Some(if remaining <= 0 {
        format!("expired on {date}")
    } else {
        match remaining / SECONDS_PER_DAY {
            0 => format!("expires on {date} (in less than a day)"),
            days => format!("expires on {date} (in {days} day(s))"),
        }
    })
}

/// `YYYY-MM-DD` in UTC, or the raw seconds when out of range.
pub(crate) fn calendar_date(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).map_or_else(
        || seconds.to_string(),
        |at| at.format("%Y-%m-%d").to_string(),
    )
}

/// The client identity files an entry names, whole or half, before a
/// (re-)enrollment replaces them: what [`settle_identity`] retires once the
/// entry names the new pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeldIdentity {
    /// `client-cert`, when the entry names one.
    pub(crate) certificate: Option<PathBuf>,
    /// `client-key`, when the entry names one.
    pub(crate) private_key: Option<PathBuf>,
}

impl HeldIdentity {
    /// Both halves, as the pair a dial presents; `None` for half or none.
    pub(crate) fn pair(&self) -> Option<ClientIdentityFiles> {
        let certificate = self.certificate.as_deref()?;
        let private_key = self.private_key.as_deref()?;
        Some(ClientIdentityFiles::existing(certificate, private_key))
    }

    /// Whether the entry names neither half.
    pub(crate) const fn is_empty(&self) -> bool {
        self.certificate.is_none() && self.private_key.is_none()
    }

    /// The far host's id for the held credential, read from a certificate
    /// (public material): the one the entry names, else the enrolled
    /// certificate beside an enrolled key the entry names alone. `None` when
    /// neither reads: a key alone cannot name it without reading private
    /// bytes.
    pub(crate) fn credential_id(&self, dir: &Path, name: &str) -> Option<String> {
        self.read_certificate(dir, name, phux_server::workload::stored_credential_id)
    }

    /// The `sha256:` fingerprint of the CA that issued the held
    /// certificate, from the chain stored with it (found as for
    /// [`Self::credential_id`]). `None` for a leaf-only or unreadable file.
    pub(crate) fn authority(&self, dir: &Path, name: &str) -> Option<String> {
        self.read_certificate(dir, name, phux_server::workload::stored_chain_authority)
    }

    /// `read` over the certificate the entry names, else the enrolled
    /// certificate beside an enrolled key the entry names alone.
    pub(super) fn read_certificate(
        &self,
        dir: &Path,
        name: &str,
        read: fn(&Path) -> Option<String>,
    ) -> Option<String> {
        self.certificate.as_deref().and_then(read).or_else(|| {
            self.enrolled_files(dir, name)
                .iter()
                .filter(|path| is_enrolled_file(path, dir, name, CERT_EXTENSION))
                .find_map(|path| read(path))
        })
    }

    /// The files an enrollment wrote for `name` under `dir` that this
    /// identity names: each enrolled half, with the other half of its pair
    /// beside it, since an entry that lost one half still left both files
    /// on disk. Files the operator named themselves are never included.
    pub(super) fn enrolled_files(&self, dir: &Path, name: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for (path, extension) in [
            (self.certificate.as_deref(), CERT_EXTENSION),
            (self.private_key.as_deref(), KEY_EXTENSION),
        ] {
            let Some(path) = path.filter(|path| is_enrolled_file(path, dir, name, extension))
            else {
                continue;
            };
            for sibling in [CERT_EXTENSION, KEY_EXTENSION] {
                let sibling = path.with_extension(sibling.trim_start_matches('.'));
                if !files.contains(&sibling) {
                    files.push(sibling);
                }
            }
        }
        files
    }
}

/// What an enrollment did about the workload client certificate: the
/// `enrollment.status` of `phux host add --json` and `phux host renew
/// --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CertificateStatus {
    /// A new certificate was enrolled.
    Enrolled,
    /// The previous certificate still works and is not due for renewal.
    Kept,
    /// Enrollment was tried and failed; the entry keeps the previous
    /// certificate, or none.
    Failed(String),
    /// Nothing was tried: a satellite, `--ssh-only`, the manual form, or a
    /// host with no direct route to present a certificate to.
    Skipped,
}

impl CertificateStatus {
    /// The stable `enrollment.status` string.
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::Enrolled => "enrolled",
            Self::Kept => "kept",
            Self::Failed(_) => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Why enrollment failed, when it did.
    pub(crate) fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            Self::Enrolled | Self::Kept | Self::Skipped => None,
        }
    }
}
