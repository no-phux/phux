//! Registering an enrolled machine: the previous credential, the route it
//! keeps, and the report of what happened to its workload identity.

use std::path::Path;
use std::process::ExitCode;

use super::super::enroll::{
    self, CertificateStatus, ClientIdentityFiles, EnrollEvent, EnrollRequest, FarHost,
    HeldIdentity, Settlement,
};
use super::super::json_err::{CliError, codes};
use super::super::remote::{self, RemoteEntry};
use super::super::satellite as satellite_registry;
use super::super::service;

use super::HostRole;
use super::add::{add_failure_error, registry_failure, reject_enrollment};
use super::hub::{add_hub_reload, reload_local_hub, report_local_hub};
use super::list::{HostRow, row_json};

/// Token, pin, and client identity already held for `name`, when
/// re-enrolling a remote.
#[derive(Default)]
pub(crate) struct PreviousEnrollment {
    pub(crate) token: Option<String>,
    pub(crate) fingerprint: Option<String>,
    /// The whole pair, presented on the reuse probe and kept when it works.
    pub(crate) identity: Option<ClientIdentityFiles>,
    /// Every client identity file the entry names, whole or half: what a
    /// new pair supersedes.
    pub(crate) held: HeldIdentity,
}

impl PreviousEnrollment {
    /// What `entry` holds.
    pub(crate) fn of(entry: &RemoteEntry) -> Self {
        let held = HeldIdentity {
            certificate: entry.client_cert.clone(),
            private_key: entry.client_key.clone(),
        };
        Self {
            token: remote::read_token(entry).ok().flatten(),
            fingerprint: entry.cert_fingerprint.clone(),
            identity: held.pair(),
            held,
        }
    }
}

pub(super) fn previous_enrollment(name: &str, role: HostRole) -> PreviousEnrollment {
    if role != HostRole::Remote {
        return PreviousEnrollment::default();
    }
    remote::find(name).map_or_else(PreviousEnrollment::default, |entry| {
        PreviousEnrollment::of(&entry)
    })
}

/// Set a registered remote up over ssh and rewrite its entry, so the attach
/// repair rung and `phux host add` register through one tail.
pub(crate) fn enroll_remote_over_ssh(
    name: &str,
    req: &EnrollRequest<'_>,
    session: Option<&str>,
    held: &HeldIdentity,
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Result<(RemoteEntry, enroll::EnrollOutcome), CliError> {
    let outcome = enroll::enroll_over_ssh(req, &mut |event| narrate(&event))
        .map_err(|failure| add_failure_error(req.ssh_host, HostRole::Remote, &failure))?;
    register_enrollment(
        HostRole::Remote,
        name,
        req,
        &outcome,
        session,
        held,
        narrate,
    )?;
    let entry = remote::find(name)
        .ok_or_else(|| registry_failure(format!("{name:?} was written but cannot be read back")))?;
    Ok((entry, outcome))
}

/// `--role satellite` has to leave this machine able to dial the new peer.
/// `--role remote` is the opposite trust direction and must not touch the
/// local unit.
pub(super) fn local_hub_for(role: HostRole) -> Option<service::LocalHub> {
    (role == HostRole::Satellite).then(service::ensure_local_hub)
}

/// Register what the ssh middle produced, then settle the workload client
/// identity ([`enroll::settle_identity`]): once the entry names a new pair,
/// the credential it held is revoked on the far host and its enrolled files
/// go (never files the operator named themselves); a new pair the entry did
/// not record is removed and its credential revoked instead. The registry
/// write is the one switch between the two pairs, so a reader always sees a
/// whole old pair or a whole new one, and the old credential is never
/// revoked before the entry stops naming it.
pub(super) fn register_enrollment(
    role: HostRole,
    name: &str,
    req: &EnrollRequest<'_>,
    outcome: &enroll::EnrollOutcome,
    session: Option<&str>,
    held: &HeldIdentity,
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Result<(HostRow, Settlement), CliError> {
    let route = Route {
        endpoint: &outcome.endpoint,
        pairing: Some(&outcome.report),
        direct: outcome.direct.as_deref(),
        identity: outcome.identity.as_ref(),
        remote_phux: Some(req.remote_phux),
    };
    let registered = finish_enroll(role, name, &route, session, Some(req.ssh_host));
    let recorded = registered
        .as_ref()
        .ok()
        .and_then(|row| row.client_cert.as_deref());
    // A role that enrolls no certificate (a satellite) has nothing to settle.
    let Some(workload) = &req.workload else {
        return registered.map(|row| (row, Settlement::default()));
    };
    let fresh = outcome.identity.as_ref();
    let settlement = enroll::settle_identity(
        &FarHost {
            ssh_host: req.ssh_host,
            remote_phux: req.remote_phux,
        },
        fresh,
        fresh.is_some_and(|identity| recorded == Some(identity.certificate.as_path())),
        held,
        workload,
        &mut |event| narrate(&event),
    );
    registered.map(|row| (row, settlement))
}

/// Where an enrolled entry points and what it authenticates with.
pub(super) struct Route<'a> {
    /// The endpoint to register.
    pub(super) endpoint: &'a str,
    /// The pairing document (`None` for `--ssh-only`).
    pub(super) pairing: Option<&'a enroll::PairReport>,
    /// A direct route kept beside an `ssh://` endpoint.
    pub(super) direct: Option<&'a str>,
    /// The workload client identity to record (remotes only).
    pub(super) identity: Option<&'a ClientIdentityFiles>,
    /// The `phux` enrollment ran there, recorded for later ssh commands
    /// (remotes only; plain `phux` is not recorded).
    pub(super) remote_phux: Option<&'a str>,
}

impl<'a> Route<'a> {
    /// An endpoint with no credentials at all (`--ssh-only`).
    pub(super) const fn bare(endpoint: &'a str) -> Self {
        Self {
            endpoint,
            pairing: None,
            direct: None,
            identity: None,
            remote_phux: None,
        }
    }
}

/// The role-specific tail of the ssh form: validate the entry, write the
/// pairing token under `remotes/` or `satellites/`, and register it.
/// Validation runs before the token hits disk so a rejected entry never
/// leaves an orphaned bearer token.
pub(super) fn finish_enroll(
    role: HostRole,
    name: &str,
    route: &Route<'_>,
    session: Option<&str>,
    ssh: Option<&str>,
) -> Result<HostRow, CliError> {
    finish_enroll_in(
        &phux_server::telemetry::state_dir(),
        role,
        name,
        route,
        session,
        ssh,
    )
}

/// [`finish_enroll`] with the state directory injectable, so tests can use a
/// tempdir (`env::set_var` is unsafe and this crate forbids `unsafe`).
pub(super) fn finish_enroll_in(
    state_dir: &Path,
    role: HostRole,
    name: &str,
    route: &Route<'_>,
    session: Option<&str>,
    ssh: Option<&str>,
) -> Result<HostRow, CliError> {
    let (endpoint, pairing) = (route.endpoint, route.pairing);
    let direct = route.direct.filter(|_| role == HostRole::Remote);
    let dialable = keeps_credentials(role, endpoint, direct);
    let token_file = (pairing.is_some() && dialable).then(|| match role {
        HostRole::Remote => enroll::token_path(state_dir, name),
        HostRole::Satellite => enroll::satellite_token_path(state_dir, name),
    });
    let cert_fingerprint = pairing
        .filter(|_| dialable)
        .and_then(|report| report.cert_fingerprint.as_deref());
    let identity = route.identity.filter(|_| dialable).map(|identity| {
        (
            identity.certificate.as_path(),
            identity.private_key.as_path(),
        )
    });

    match role {
        HostRole::Remote => {
            let new = remote::NewRemote::new(
                name,
                endpoint,
                token_file.as_deref(),
                cert_fingerprint,
                session,
            )
            .and_then(|new| {
                new.with_ssh(ssh)
                    .with_remote_phux(route.remote_phux)
                    .with_direct(direct)
            })
            .and_then(|new| new.with_client_identity(identity))
            .map_err(reject_enrollment)?;
            write_pairing_token(token_file.as_deref(), pairing)?;
            remote::add_or_update(&new).map_err(registry_failure)?;
            // Pinned over ssh, a channel the operator already trusts
            // (ADR-0153): trust on first pair, not first connect.
            remote::pin_authority(
                name,
                cert_fingerprint,
                None,
                pairing.and_then(|report| report.ca_fingerprint.as_deref()),
            );
            Ok(HostRow::from_new_remote(new))
        }
        HostRole::Satellite => {
            let new = satellite_registry::NewSatellite::new(
                name,
                endpoint,
                true,
                token_file.as_deref(),
                cert_fingerprint,
            )
            .map_err(reject_enrollment)?;
            write_pairing_token(token_file.as_deref(), pairing)?;
            let entry = satellite_registry::add_or_update(&new).map_err(registry_failure)?;
            Ok(HostRow::from_satellite(entry))
        }
    }
}

/// Whether an enrolled entry stores its token and pin: only for a dialed
/// transport, or an `ssh://` remote that keeps a direct route to promote.
pub(super) fn keeps_credentials(role: HostRole, endpoint: &str, direct: Option<&str>) -> bool {
    !endpoint.starts_with("ssh://") || (role == HostRole::Remote && direct.is_some())
}

/// Write the pairing token owner-only, when the entry has one to write.
pub(super) fn write_pairing_token(
    path: Option<&Path>,
    pairing: Option<&enroll::PairReport>,
) -> Result<(), CliError> {
    if let (Some(path), Some(report)) = (path, pairing) {
        enroll::write_token(path, &report.token).map_err(|err| {
            CliError::new(
                codes::REGISTRY,
                err,
                "check the phux state directory is writable, then rerun \
                 `phux host add`",
            )
        })?;
    }
    Ok(())
}

/// Report a registered machine: the `"host"` document under `--json`, the
/// human summary otherwise. `hub` is `Some` only for `--role satellite`.
pub(super) fn report_registered(
    row: &HostRow,
    how: &str,
    notes: &[String],
    json: bool,
    hub: Option<&service::LocalHub>,
    enrollment: &EnrollmentReport,
) -> ExitCode {
    let reload = (row.role == HostRole::Satellite).then(reload_local_hub);
    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "host": row_json(row),
            "enrollment": enrollment.to_json(row.client_cert.as_deref()),
        });
        if let Some(hub) = hub {
            doc["hub_service"] = serde_json::Value::String(hub.as_json_str().to_owned());
        }
        add_hub_reload(&mut doc, reload.as_ref());
        return crate::output::json(&doc);
    }
    let role = match row.role {
        HostRole::Remote => "",
        HostRole::Satellite => "satellite ",
    };
    outln!("Registered {role}{} -> {}  ({how})", row.name, row.endpoint);
    for note in notes {
        outln!("  {note}");
    }
    match row.role {
        HostRole::Remote => {
            let name = &row.name;
            let transport = if row.endpoint.starts_with("ssh://") {
                "attach (over ssh)"
            } else {
                "attach (direct, no ssh in the path)"
            };
            let attach_cmd = format!("phux attach {name}");
            let ls_cmd = format!("phux ls --remote {name}");
            let host_ls_cmd = "phux host ls";
            // One column for the three rows, sized to the longest command,
            // so a long host name never runs into its description.
            let col = [attach_cmd.len(), ls_cmd.len(), host_ls_cmd.len()]
                .into_iter()
                .max()
                .unwrap_or(0)
                + 2;
            outln!("  {attach_cmd:<col$}{transport}");
            outln!("  {ls_cmd:<col$}list its sessions");
            outln!("  {host_ls_cmd:<col$}every registered machine");
        }
        HostRole::Satellite => report_local_hub(hub, reload.as_ref()),
    }
    ExitCode::SUCCESS
}

/// What `host add` or `host renew` did about the workload client
/// certificate: the `enrollment` object of their `--json` documents.
pub(super) struct EnrollmentReport {
    pub(super) status: CertificateStatus,
    pub(super) settlement: Settlement,
    /// Every warning narrated along the way, in order.
    pub(super) warnings: Vec<String>,
}

impl EnrollmentReport {
    pub(super) const fn new(status: CertificateStatus) -> Self {
        Self {
            status,
            settlement: Settlement {
                previous_credential_id: None,
                previous_revoked: None,
            },
            warnings: Vec::new(),
        }
    }

    /// The document. `credential_id` and `expires_at` describe the
    /// certificate the entry names now (`client_cert`), public material read
    /// from it; `null` when it names none.
    pub(super) fn to_json(&self, client_cert: Option<&Path>) -> serde_json::Value {
        serde_json::json!({
            "status": self.status.as_str(),
            "error": self.status.error(),
            "credential_id": client_cert.and_then(phux_server::workload::stored_credential_id),
            "expires_at": client_cert.and_then(enroll::admission_ends),
            "previous_credential_id": self.settlement.previous_credential_id,
            "previous_revoked": self.settlement.previous_revoked,
            "warnings": self.warnings,
        })
    }
}
