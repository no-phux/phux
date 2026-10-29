//! `enroll_over_ssh`: the end-to-end enrollment of one far host.

use super::events::{EnrollEvent, EnrollFailure, Retired, Supervision};
use super::identity::{CertificateStatus, ClientIdentityFiles, EnrollRequest};
use super::pair::{PairReport, candidate_endpoints, pair_over_ssh, probe};
use super::server::{
    ensure_remote_server_after_version, remote_phux_version, try_reuse_previous_credential,
};
use super::ssh::{ssh_hostname, ssh_run};
use super::workload::{NO_SUCH_CREDENTIAL, enroll_client_certificate};

/// What the shared ssh middle produced.
pub(crate) struct EnrollOutcome {
    /// The endpoint to register: the first direct route that answered, else
    /// `ssh://HOST`.
    pub(crate) endpoint: String,
    /// When `endpoint` is `ssh://`, the first direct candidate, kept so a
    /// later attach can try it and promote it once it answers.
    pub(crate) direct: Option<String>,
    /// Every direct route that was dialed, in order, for the report.
    pub(crate) tried: Vec<String>,
    /// The parsed `phux pair --json` document (token, fingerprint, overlay
    /// addresses).
    pub(crate) report: PairReport,
    /// How the remote server is kept alive.
    pub(crate) supervision: Option<Supervision>,
    /// The workload client identity to register: freshly enrolled, the
    /// previous one kept, or none.
    pub(crate) identity: Option<ClientIdentityFiles>,
    /// What happened to the workload client certificate, for the report.
    pub(crate) certificate: CertificateStatus,
}

/// The shared middle of every ssh enrollment. Role-agnostic: nothing here
/// touches a registry or writes a token; a workload client identity, when
/// requested, is written to its own new files and handed back.
pub(crate) fn enroll_over_ssh(
    req: &EnrollRequest<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Result<EnrollOutcome, EnrollFailure> {
    let version = remote_phux_version(req.ssh_host, req.remote_phux)?;
    on_event(EnrollEvent::PhuxFound {
        version: version.trim().to_owned(),
    });

    let supervision = match ensure_remote_server_after_version(
        req.ssh_host,
        req.remote_phux,
        req.quic_port,
        req.service,
    ) {
        Ok(supervision) => {
            on_event(EnrollEvent::ServerReady(supervision.clone()));
            Some(supervision)
        }
        Err(error) => {
            on_event(EnrollEvent::ServerStartFailed { error });
            None
        }
    };

    // Reuse the credential already held before minting another live bearer
    // (ADR-0122).
    let (report, reused) = if let Some(report) = try_reuse_previous_credential(req, on_event) {
        on_event(EnrollEvent::CredentialReused);
        (report, true)
    } else {
        let report = pair_over_ssh(req.ssh_host, req.remote_phux, req.previous_token)?;
        if req.previous_token.is_some() {
            on_event(EnrollEvent::CredentialReplaced);
        } else {
            on_event(EnrollEvent::Paired);
        }
        (report, false)
    };

    let ssh_target_host = ssh_hostname(req.ssh_host);
    let candidates = candidate_endpoints(
        &ssh_target_host,
        &report,
        req.endpoint_override,
        req.quic_port,
    );
    // Only a host with a dialable route has anything to present a
    // certificate to.
    let (identity, certificate) = if candidates.is_empty() {
        let kept = req.previous_identity.cloned();
        let status = if kept.is_some() {
            CertificateStatus::Kept
        } else {
            CertificateStatus::Skipped
        };
        (kept, status)
    } else {
        client_identity_for(req, reused, on_event)
    };
    let (endpoint, direct, tried) = first_answering_route(
        req.ssh_host,
        &candidates,
        &report,
        identity.as_ref(),
        on_event,
    );
    Ok(EnrollOutcome {
        endpoint,
        direct,
        tried,
        report,
        supervision,
        identity,
        certificate,
    })
}

/// Dial each candidate in turn; the first that answers is the endpoint.
/// Nothing answering registers `ssh://HOST` with the first candidate kept as
/// the direct route to promote later. Returns `(endpoint, direct, tried)`.
pub(super) fn first_answering_route(
    ssh_host: &str,
    candidates: &[String],
    report: &PairReport,
    identity: Option<&ClientIdentityFiles>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> (String, Option<String>, Vec<String>) {
    let presented = identity.map(ClientIdentityFiles::tls);
    let mut tried = Vec::new();
    for candidate in candidates {
        // Only QUIC can be probed here; a `wss://` override is registered
        // as the operator wrote it, as it always was.
        let Some(target) = candidate.strip_prefix("quic://") else {
            return (candidate.clone(), None, tried);
        };
        on_event(EnrollEvent::Probing {
            endpoint: candidate.clone(),
        });
        tried.push(candidate.clone());
        match probe(
            target,
            &report.token,
            report.cert_fingerprint.as_deref(),
            &report.authority_pin(),
            presented.clone(),
        ) {
            Ok(()) => {
                on_event(EnrollEvent::DirectReachable {
                    endpoint: candidate.clone(),
                });
                return (candidate.clone(), None, tried);
            }
            Err(reason) => on_event(EnrollEvent::DirectUnreachable {
                endpoint: candidate.clone(),
                reason,
            }),
        }
    }
    (
        format!("ssh://{ssh_host}"),
        candidates.first().cloned(),
        tried,
    )
}

/// The scope `phux host add` enrolls with: every verb on the whole host.
/// Enrollment grants no authority ssh did not already grant, and an operator
/// who can run `phux` over ssh there already holds all of it.
///
/// Pairing records a human principal explicitly, independently of the
/// spelled-out ceiling as the permission vocabulary grows. An older host
/// that cannot enroll it reports its refusal; we never retry with less scope.
pub(crate) const HOST_ADD_WORKLOAD_SCOPE: &str = phux_server::workload::PAIRED_DEVICE_SCOPE;

/// The workload identity to register: the previous one when its bearer
/// still worked (nothing was re-minted) and it is not due for renewal,
/// otherwise a fresh enrollment. The previous credential stays live on the
/// far host until [`settle_identity`] runs after the registry write. A
/// failed enrollment is a warning, never a failed `host add`: the host is
/// paired either way, and the previous identity (if any) is kept.
pub(super) fn client_identity_for(
    req: &EnrollRequest<'_>,
    reused: bool,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> (Option<ClientIdentityFiles>, CertificateStatus) {
    if reused && let Some(previous) = req.previous_identity {
        match previous.renewal_due(chrono::Utc::now().timestamp()) {
            None => return (Some(previous.clone()), CertificateStatus::Kept),
            Some(detail) => on_event(EnrollEvent::CertificateRenewalDue { detail }),
        }
    }
    let Some(workload) = req.workload.as_ref() else {
        return (None, CertificateStatus::Skipped);
    };
    let far = FarHost {
        ssh_host: req.ssh_host,
        remote_phux: req.remote_phux,
    };
    match enroll_client_certificate(&far, workload, on_event) {
        Ok(identity) => {
            on_event(EnrollEvent::CertificateEnrolled {
                credential_id: identity.credential_id.clone().unwrap_or_default(),
            });
            (Some(identity), CertificateStatus::Enrolled)
        }
        Err(reason) => {
            on_event(EnrollEvent::CertificateNotEnrolled {
                reason: reason.clone(),
            });
            (
                req.previous_identity.cloned(),
                CertificateStatus::Failed(reason),
            )
        }
    }
}

/// The far end of an enrollment: where ssh goes and which `phux` runs there.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FarHost<'a> {
    pub(crate) ssh_host: &'a str,
    pub(crate) remote_phux: &'a str,
}

impl FarHost<'_> {
    /// The command an operator runs to revoke `credential_id` by hand.
    pub(super) fn revoke_command(&self, credential_id: &str) -> String {
        format!(
            "ssh {} {} workload revoke {credential_id}",
            self.ssh_host, self.remote_phux
        )
    }

    /// Revoke `credential_id` on the far host: `Ok(true)` once it is
    /// revoked (now or before), `Ok(false)` when the host holds no such
    /// credential (it admits nothing there, but it may still be live on
    /// another host). The id is public (a hash of a public key); nothing
    /// secret enters argv.
    pub(super) fn revoke(&self, credential_id: &str) -> Result<bool, String> {
        let out = ssh_run(
            self.ssh_host,
            &[
                self.remote_phux,
                "workload",
                "revoke",
                credential_id,
                "--json",
            ],
        )?;
        if out.status.success() {
            return Ok(true);
        }
        if out.stderr.contains(NO_SUCH_CREDENTIAL) {
            return Ok(false);
        }
        Err(out.failure_detail())
    }

    /// Revoke `credential_id`, narrating the result: `Some(true)` when it
    /// is revoked, `Some(false)` when the revocation failed, `None` when
    /// this host never held it. A superseded credential this host does not
    /// hold is reported (the entry may have pointed at another host); an
    /// unrecorded one it does not hold was never enrolled, so says nothing.
    pub(super) fn retire(
        &self,
        credential_id: &str,
        retired: Retired,
        on_event: &mut dyn FnMut(EnrollEvent),
    ) -> Option<bool> {
        let credential_id = credential_id.to_owned();
        match self.revoke(&credential_id) {
            Ok(true) => {
                on_event(EnrollEvent::CertificateRevoked {
                    credential_id,
                    retired,
                });
                Some(true)
            }
            Ok(false) => {
                if retired == Retired::Superseded {
                    on_event(EnrollEvent::CertificateNotHeld { credential_id });
                }
                None
            }
            Err(reason) => {
                on_event(EnrollEvent::CertificateNotRevoked {
                    remedy: self.revoke_command(&credential_id),
                    credential_id,
                    retired,
                    reason,
                });
                Some(false)
            }
        }
    }
}
