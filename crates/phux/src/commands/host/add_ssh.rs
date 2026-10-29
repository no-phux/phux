//! `phux host add [USER@]HOST`: setting a machine up over ssh.

use std::process::ExitCode;

use super::super::enroll::{
    self, CertificateStatus, EnrollEvent, EnrollRequest, ServicePolicy, WorkloadEnrollment,
};
use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self, Endpoint, RemoteEntry};
use super::super::remote_target::RemoteTarget;

use super::HostRole;
use super::add::add_failure_error;
use super::args::AddOpts;
use super::enrollment::{
    EnrollmentReport, Route, finish_enroll, local_hub_for, previous_enrollment,
    register_enrollment, report_registered,
};
use super::list::HostRow;

/// The ssh form of `phux host add`: set the machine up end to end.
pub(super) fn run_add_over_ssh(raw_target: &str, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    let target = match RemoteTarget::parse_labeled(raw_target, "host add") {
        Ok(target) => target,
        Err(err) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::REGISTRY,
                    err,
                    "name the machine as you would after `ssh`: `phux host add me@mini`",
                ),
                2,
            );
        }
    };
    // `ssh` takes the typed `user@host` spelling as its destination.
    let ssh_host = target.registry_name();
    let name = opts.name.clone().unwrap_or_else(|| target.host.clone());
    let quic_port = target.port.unwrap_or(opts.quic_port);

    if opts.ssh_only {
        return run_add_ssh_only(&name, &ssh_host, opts);
    }

    if opts.accept_new_host_key {
        enroll::accept_new_host_keys();
    }

    let mut warnings = Vec::new();
    let mut narrate = |event: &EnrollEvent| {
        if event.is_warning() {
            warnings.push(event.describe());
        }
        if !json {
            event.report_progress(&name);
        }
    };

    if opts.role == HostRole::Remote
        && let Some(row) = already_current(&name, &mut narrate)
    {
        return report_registered(
            &row,
            "already registered and answering; nothing to do",
            &[],
            json,
            None,
            &EnrollmentReport::new(CertificateStatus::Kept),
        );
    }

    // An answering entry is left alone above; only a host being set up
    // gets phux installed or updated.
    let remote_phux = match provisioned_phux(&name, &ssh_host, opts, &mut narrate) {
        Ok(phux) => phux,
        Err(err) => return json_err::emit(json, &err, 1),
    };

    // Prefer credentials already on file for this name when re-enrolling so
    // a stopped server does not mint another live bearer into remote-tokens.
    let previous = previous_enrollment(&name, opts.role);
    let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
    let req = EnrollRequest {
        ssh_host: &ssh_host,
        remote_phux: &remote_phux,
        endpoint_override: opts.endpoint_override.as_deref(),
        quic_port,
        service: if opts.no_service {
            ServicePolicy::Skip
        } else {
            ServicePolicy::Install
        },
        previous_token: previous.token.as_deref(),
        previous_fingerprint: previous.fingerprint.as_deref(),
        previous_identity: previous.identity.as_ref(),
        workload: (opts.role == HostRole::Remote).then(|| WorkloadEnrollment {
            dir: &remotes_dir,
            name: &name,
        }),
    };
    let outcome = match enroll::enroll_over_ssh(&req, &mut |event| narrate(&event)) {
        Ok(outcome) => outcome,
        Err(failure) => {
            return json_err::emit(json, &add_failure_error(&ssh_host, opts.role, &failure), 1);
        }
    };

    let registered = register_enrollment(
        opts.role,
        &name,
        &req,
        &outcome,
        opts.session.as_deref(),
        &previous.held,
        &mut narrate,
    );
    match registered {
        Ok((row, settlement)) => {
            // A kept identity the entry could not record (nothing
            // dialable) is not presented: nothing was kept.
            let status = match &outcome.certificate {
                CertificateStatus::Kept if row.client_cert.is_none() => CertificateStatus::Skipped,
                status => status.clone(),
            };
            let enrollment = EnrollmentReport {
                status,
                settlement,
                warnings,
            };
            report_ssh_add(&row, &outcome, &enrollment, &ssh_host, quic_port, opts)
        }
        Err(err) => json_err::emit(json, &err, 1),
    }
}

/// The `phux` to enroll with: `--remote-phux`, else the one the entry
/// recorded, else `phux`; under `--install`, the one put on (or updated on)
/// the host first.
fn provisioned_phux(
    name: &str,
    ssh_host: &str,
    opts: &AddOpts,
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Result<String, CliError> {
    let remote_phux = if opts.remote_phux == "phux" && opts.role == HostRole::Remote {
        remote::find(name).map_or_else(|| "phux".to_owned(), |entry| entry.remote_phux().to_owned())
    } else {
        opts.remote_phux.clone()
    };
    if !opts.install {
        return Ok(remote_phux);
    }
    enroll::provision_remote_phux(ssh_host, &remote_phux, &mut |event| narrate(&event))
        .map_err(|failure| add_failure_error(ssh_host, opts.role, &failure))
}

/// A registered remote whose saved route answers and whose workload client
/// certificate is not due for renewal needs nothing: its row, so `host add`
/// can say so and stop rather than re-pair it. One without a certificate,
/// with one due, or whose route does not answer is narrated and set up
/// again (`None`).
pub(super) fn already_current(
    name: &str,
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Option<HostRow> {
    let entry = remote::find(name)?;
    let (cert, _) = entry.client_identity_paths()?;
    let renewal = enroll::renewal_due(cert, chrono::Utc::now().timestamp());
    match (saved_route_answers(&entry), renewal) {
        (Some(Ok(())), None) => return Some(HostRow::from_remote(entry)),
        (Some(Ok(())), Some(detail)) => {
            narrate(&EnrollEvent::CertificateRenewalDue { detail });
        }
        (Some(Err(reason)), _) => narrate(&EnrollEvent::DirectUnreachable {
            endpoint: entry.endpoint.clone(),
            reason: format!("{reason}; setting the machine up again over ssh"),
        }),
        (None, _) => {}
    }
    None
}

/// `--ssh-only` skips the host entirely: no pairing, no service, no
/// certificate, just a registry entry riding existing ssh trust.
pub(super) fn run_add_ssh_only(name: &str, ssh_host: &str, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    let endpoint = format!("ssh://{ssh_host}");
    let route = Route {
        remote_phux: Some(&opts.remote_phux),
        ..Route::bare(&endpoint)
    };
    let registered = finish_enroll(
        opts.role,
        name,
        &route,
        opts.session.as_deref(),
        Some(ssh_host),
    );
    match registered {
        Ok(row) => report_registered(
            &row,
            "rides your ssh trust; nothing on the host was touched",
            &[],
            json,
            local_hub_for(opts.role).as_ref(),
            &EnrollmentReport::new(CertificateStatus::Skipped),
        ),
        Err(err) => json_err::emit(json, &err, 1),
    }
}

/// The summary after the ssh form registered a machine: how it connects,
/// what was tried when nothing answered, and what to type next.
pub(super) fn report_ssh_add(
    row: &HostRow,
    outcome: &enroll::EnrollOutcome,
    enrollment: &EnrollmentReport,
    ssh_host: &str,
    quic_port: u16,
    opts: &AddOpts,
) -> ExitCode {
    let supervision = outcome.supervision.as_ref().map_or_else(
        || "no server could be started".to_owned(),
        enroll::Supervision::describe,
    );
    let rides_ssh = outcome.endpoint.starts_with("ssh://");
    let how = if rides_ssh {
        format!("paired; no direct route answered, so attaches ride ssh; {supervision}")
    } else {
        format!("paired; {supervision}")
    };
    let mut notes = Vec::new();
    if rides_ssh {
        if !outcome.tried.is_empty() {
            notes.push(format!("tried {}", outcome.tried.join(", ")));
        }
        notes.push(format!(
            "rerun `phux host add {ssh_host}` once UDP {quic_port} is open between here and there; \
             an attach also tries the direct route first and upgrades the entry when it answers"
        ));
    }
    report_registered(
        row,
        &how,
        &notes,
        opts.json.json,
        local_hub_for(opts.role).as_ref(),
        enrollment,
    )
}

/// Whether a registered entry's saved direct route answers right now.
/// `None` when there is nothing to dial without ssh (an `ssh://` route, a
/// `wss://` one the QUIC probe cannot speak, or missing credentials).
pub(super) fn saved_route_answers(entry: &RemoteEntry) -> Option<Result<(), String>> {
    let Ok(Endpoint::Quic(target)) = Endpoint::parse(&entry.endpoint) else {
        return None;
    };
    let token = remote::read_token(entry).ok().flatten()?;
    let identity = entry.client_identity().ok()?;
    Some(enroll::probe(
        &target,
        &token,
        entry.cert_fingerprint.as_deref(),
        &entry.authority_pin(),
        identity,
    ))
}
