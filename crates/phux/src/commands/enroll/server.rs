//! Making sure the far host runs a phux server, and reusing a credential it
//! already accepts.

use super::SERVICE_INCUMBENT_LIVE;
use super::events::{EnrollEvent, EnrollFailure, ServicePolicy, Supervision};
use super::identity::{ClientIdentityFiles, EnrollRequest};
use super::pair::{PairReport, candidate_endpoints, probe};
use super::ssh::{COMMAND_NOT_FOUND, SSH_FAILED, ssh_hostname, ssh_run};

/// Make sure a server is running on `ssh_host` and, under
/// [`ServicePolicy::Install`], that it keeps running. Confirms `phux` first so a
/// lost install is reported as that.
pub(crate) fn ensure_remote_server(
    ssh_host: &str,
    remote_phux: &str,
    quic_port: u16,
    policy: ServicePolicy,
) -> Result<Supervision, EnrollFailure> {
    remote_phux_version(ssh_host, remote_phux)?;
    ensure_remote_server_after_version(ssh_host, remote_phux, quic_port, policy)
        .map_err(EnrollFailure::Pair)
}

/// [`ensure_remote_server`] once `phux` is known to be there. Under
/// [`ServicePolicy::Install`], `phux service install --quic` covers every state:
/// no unit (written and started), a stopped unit (reloaded), or a live server
/// holding the socket (refused, then answered with `--adopt`). Anything else
/// falls back to an unsupervised `phux server --ensure`.
pub(super) fn ensure_remote_server_after_version(
    ssh_host: &str,
    remote_phux: &str,
    quic_port: u16,
    policy: ServicePolicy,
) -> Result<Supervision, String> {
    let quic_bind = format!("0.0.0.0:{quic_port}");
    let install_error = match policy {
        ServicePolicy::Skip => None,
        ServicePolicy::Install => {
            let install = ssh_run(
                ssh_host,
                &[remote_phux, "service", "install", "--quic", &quic_bind],
            )?;
            if install.status.success() {
                return Ok(Supervision::Service);
            }
            if install.stderr.contains(SERVICE_INCUMBENT_LIVE) {
                let adopt = ssh_run(
                    ssh_host,
                    &[
                        remote_phux,
                        "service",
                        "install",
                        "--quic",
                        &quic_bind,
                        "--adopt",
                    ],
                )?;
                if adopt.status.success() {
                    return Ok(Supervision::Adopted);
                }
                Some(adopt.failure_detail())
            } else {
                Some(install.failure_detail())
            }
        }
    };

    let ensure = ssh_run(ssh_host, &[remote_phux, "server", "--ensure"])?;
    if !ensure.status.success() {
        let detail = ensure.failure_detail();
        return Err(install_error.map_or_else(
            || format!("`phux server --ensure` failed: {detail}"),
            |install| {
                format!(
                    "service install failed ({install}) and `phux server --ensure` failed ({detail})"
                )
            },
        ));
    }
    Ok(
        install_error.map_or(Supervision::Skipped, |reason| Supervision::Unsupervised {
            reason,
        }),
    )
}

/// After the server is up, dial with the previous credential; `Some` when a
/// direct route accepts it.
pub(super) fn try_reuse_previous_credential(
    req: &EnrollRequest<'_>,
    on_event: &mut dyn FnMut(EnrollEvent),
) -> Option<PairReport> {
    let token = req.previous_token?;
    let fingerprint = req.previous_fingerprint.filter(|fp| !fp.is_empty())?;
    let report = PairReport {
        token: token.to_owned(),
        cert_fingerprint: Some(fingerprint.to_owned()),
        // What this client already pinned for that leaf, kept as it was.
        ca_fingerprint: phux_config::known_authorities::lookup(
            &super::super::remote::known_authorities(),
            fingerprint,
            None,
        ),
        overlay_addresses: Vec::new(),
    };
    let presented = req.previous_identity.map(ClientIdentityFiles::tls);
    let ssh_target_host = ssh_hostname(req.ssh_host);
    let candidates = candidate_endpoints(
        &ssh_target_host,
        &report,
        req.endpoint_override,
        req.quic_port,
    );
    for candidate in &candidates {
        let Some(target) = candidate.strip_prefix("quic://") else {
            // A `wss://` override cannot be probed here; keep the previous
            // credential and let the caller register it as written.
            return Some(report);
        };
        on_event(EnrollEvent::Probing {
            endpoint: candidate.clone(),
        });
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
                return Some(report);
            }
            Err(reason) => on_event(EnrollEvent::DirectUnreachable {
                endpoint: candidate.clone(),
                reason,
            }),
        }
    }
    None
}

/// Confirm `phux` is on the remote `PATH` before doing anything that assumes
/// it, so the failure names the real problem instead of surfacing as a
/// baffling empty pair document. Returns the version line.
pub(crate) fn remote_phux_version(
    ssh_host: &str,
    remote_phux: &str,
) -> Result<String, EnrollFailure> {
    let output =
        ssh_run(ssh_host, &[remote_phux, "--version"]).map_err(EnrollFailure::SshUnreachable)?;
    match output.status.code() {
        Some(0) => Ok(output.stdout),
        Some(SSH_FAILED) => Err(EnrollFailure::SshUnreachable(output.failure_detail())),
        Some(COMMAND_NOT_FOUND) => Err(EnrollFailure::MissingPhux(format!(
            "`{remote_phux}` was not found on {ssh_host}"
        ))),
        _ => Err(EnrollFailure::MissingPhux(
            output.command_failure(ssh_host, &[remote_phux, "--version"]),
        )),
    }
}
