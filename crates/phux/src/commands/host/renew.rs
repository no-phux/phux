//! `phux host renew` and `phux host upgrade`.

use std::process::ExitCode;

use super::super::enroll::{
    self, CertificateStatus, ClientIdentityFiles, EnrollEvent, FarHost, HeldIdentity,
    WorkloadEnrollment,
};
use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self, Endpoint, RemoteEntry};
use super::super::remote_target::{QuicUpgrade, upgrade_to_quic};

use super::HostRole;
use super::add::{registry_failure, reject_enrollment};
use super::enrollment::{EnrollmentReport, PreviousEnrollment};
use super::list::{HostRow, row_json};
use super::manage::find_host;

/// `phux host renew NAME`: a fresh workload client certificate for a
/// registered remote, over the ssh destination it was enrolled through.
/// Two-phase like `host add`: the entry is rewritten to the new pair first,
/// then the credential it held is revoked on the far host; any failure
/// before the rewrite leaves the entry on its current certificate. The new
/// certificate must come from the authority that issued the held one, so a
/// destination that reaches a different machine changes nothing.
pub(super) fn run_renew(name: &str, remote_phux: &str, json: bool) -> ExitCode {
    let plan = match RenewPlan::for_entry(name) {
        Ok(plan) => plan,
        Err((err, code)) => return json_err::emit(json, &err, code),
    };
    let ssh_host = plan.entry.ssh_destination();
    // `--remote-phux` left at its default defers to the one the entry
    // recorded at enrollment.
    let remote_phux = if remote_phux == "phux" {
        plan.entry.remote_phux().to_owned()
    } else {
        remote_phux.to_owned()
    };
    let remote_phux = remote_phux.as_str();
    let far = FarHost {
        ssh_host: &ssh_host,
        remote_phux,
    };
    let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
    let workload = WorkloadEnrollment {
        dir: &remotes_dir,
        name,
    };
    let mut warnings = Vec::new();
    let mut narrate = |event: EnrollEvent| {
        if event.is_warning() {
            warnings.push(event.describe());
        }
        if !json {
            event.report_progress(name);
        }
    };

    let fresh = match enroll::enroll_client_certificate(&far, &workload, &mut narrate) {
        Ok(fresh) => fresh,
        Err(reason) => {
            let err = CliError::new(
                codes::WORKLOAD,
                format!("could not renew the workload client certificate for {name}: {reason}"),
                format!(
                    "the entry still presents its current certificate; check `ssh {ssh_host} {remote_phux} --version` and rerun `phux host renew {name}`"
                ),
            );
            return json_err::emit(json, &err, 1);
        }
    };
    narrate(EnrollEvent::CertificateEnrolled {
        credential_id: fresh.credential_id.clone().unwrap_or_default(),
    });
    let written = plan.record(&fresh, &ssh_host);
    let settlement = enroll::settle_identity(
        &far,
        Some(&fresh),
        written.is_ok(),
        &plan.held,
        &workload,
        &mut narrate,
    );
    let row = match written {
        Ok(new) => HostRow::from_new_remote(new),
        Err(err) => return json_err::emit(json, &err, 1),
    };
    let enrollment = EnrollmentReport {
        status: CertificateStatus::Enrolled,
        settlement,
        warnings,
    };
    report_renewed(&row, &enrollment, json)
}

/// What `host renew` checked before touching the far host: the entry, the
/// identity it holds, and the authority that issued it.
pub(super) struct RenewPlan {
    pub(super) entry: RemoteEntry,
    /// The entry revalidated, so a bad field refuses before any ssh.
    pub(super) rewritten: remote::NewRemote,
    pub(super) held: HeldIdentity,
    /// The CA fingerprint of the held certificate's chain, when it has one.
    pub(super) authority: Option<String>,
}

impl RenewPlan {
    pub(super) fn for_entry(name: &str) -> Result<Self, (CliError, u8)> {
        find_host(name, Some(HostRole::Remote), "renew")?;
        let entry = remote::find(name).ok_or_else(|| {
            (
                registry_failure(format!("{name:?} vanished from the registry")),
                1,
            )
        })?;
        let held = PreviousEnrollment::of(&entry).held;
        let refuse = |message: String, remedy: String| {
            Err((CliError::new(codes::REGISTRY, message, remedy), 2))
        };
        let add = format!("phux host add {}", entry.ssh_destination());
        if held.is_empty() {
            return refuse(
                format!("{name} has no workload client certificate to renew"),
                format!("`{add}` sets the machine up and enrolls one"),
            );
        }
        let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
        let authority = held.authority(&remotes_dir, name);
        // With no issuing CA to compare against, only a destination the
        // entry recorded (or its ssh:// endpoint) says which host to ask.
        if authority.is_none()
            && entry.ssh.is_none()
            && !matches!(Endpoint::parse(&entry.endpoint), Ok(Endpoint::Ssh(_)))
        {
            return refuse(
                format!(
                    "{name} records no ssh destination and its certificate names no issuing authority, so renewing could enroll with the wrong host"
                ),
                format!("`{add}` sets it up again through the destination you name"),
            );
        }
        let rewritten =
            remote::NewRemote::from_entry(&entry).map_err(|err| (reject_enrollment(err), 2))?;
        Ok(Self {
            entry,
            rewritten,
            held,
            authority,
        })
    }

    /// Rewrite the entry to `fresh`, refusing a certificate from another
    /// authority than the held one's. Nothing is written on refusal.
    pub(super) fn record(
        &self,
        fresh: &ClientIdentityFiles,
        ssh_host: &str,
    ) -> Result<remote::NewRemote, CliError> {
        let issued_by = phux_server::workload::stored_chain_authority(&fresh.certificate);
        if let Some(held) = &self.authority
            && issued_by.as_ref() != Some(held)
        {
            return Err(CliError::new(
                codes::WORKLOAD,
                format!(
                    "{ssh_host} issued the new certificate from a different workload authority than the one {} holds; the entry is unchanged",
                    self.entry.name
                ),
                format!(
                    "check that `{ssh_host}` reaches the machine {} dials, or set it up again with `phux host add`",
                    self.entry.name
                ),
            ));
        }
        let new = self
            .rewritten
            .clone()
            .with_client_identity(Some((
                fresh.certificate.as_path(),
                fresh.private_key.as_path(),
            )))
            .map_err(reject_enrollment)?;
        remote::add_or_update(&new).map_err(registry_failure)?;
        Ok(new)
    }
}

/// The summary after `host renew`: the `"host"` and `"enrollment"` document
/// under `--json`, one line otherwise.
pub(super) fn report_renewed(row: &HostRow, enrollment: &EnrollmentReport, json: bool) -> ExitCode {
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "host": row_json(row),
            "enrollment": enrollment.to_json(row.client_cert.as_deref()),
        }));
    }
    let cert = row.client_cert.as_deref();
    let id = cert
        .and_then(phux_server::workload::stored_credential_id)
        .unwrap_or_default();
    let until = cert
        .and_then(enroll::admission_ends)
        .map_or_else(String::new, |ends| {
            format!(", admitted until {}", enroll::calendar_date(ends))
        });
    outln!(
        "Renewed the workload client certificate for {}: {id}{until}",
        row.name
    );
    ExitCode::SUCCESS
}

/// `phux host upgrade [NAME]`: move every remote (or NAME) whose QUIC route
/// answers onto it, and report each one. Exits nonzero when a route that
/// should have answered did not, or its rewrite failed.
pub(super) fn run_upgrade(name: Option<&str>, json: bool) -> ExitCode {
    let entries = match remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(json, &registry_failure(err), 1),
    };
    let mut picked = entries;
    if let Some(name) = name {
        picked.retain(|entry| entry.name == name);
        if picked.is_empty() {
            let err = CliError::new(
                codes::REGISTRY,
                format!("no remote host named {name:?}"),
                "`phux host ls --role remote` lists remotes",
            );
            return json_err::emit(json, &err, 1);
        }
    }
    let deadline = enroll::probe_deadline();
    let results: Vec<(RemoteEntry, QuicUpgrade)> = picked
        .into_iter()
        .map(|entry| {
            let outcome = upgrade_to_quic(&entry, deadline);
            (entry, outcome)
        })
        .collect();
    let failed = results.iter().any(|(_, outcome)| {
        matches!(
            outcome,
            QuicUpgrade::Unreachable { .. } | QuicUpgrade::NotSaved { .. }
        )
    });
    if json {
        let hosts: Vec<_> = results
            .iter()
            .map(|(entry, outcome)| upgrade_json(entry, outcome))
            .collect();
        let written =
            crate::output::json(&serde_json::json!({"schema_version": 1, "hosts": hosts}));
        return if failed { ExitCode::FAILURE } else { written };
    }
    if results.is_empty() {
        outln!("no remote hosts registered; `phux host add HOST` adds one");
    } else {
        for (entry, outcome) in &results {
            outln!("{}", upgrade_line(entry, outcome));
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// One `host upgrade` result as a stable JSON object: `status` is
/// `upgraded`, `already_quic`, `no_route`, `unreachable`, or `not_saved`,
/// and `endpoint` is what the entry dials now.
pub(super) fn upgrade_json(entry: &RemoteEntry, outcome: &QuicUpgrade) -> serde_json::Value {
    let (status, endpoint, candidate, error) = match outcome {
        QuicUpgrade::AlreadyQuic => ("already_quic", entry.endpoint.as_str(), None, None),
        QuicUpgrade::NoRoute => ("no_route", entry.endpoint.as_str(), None, None),
        QuicUpgrade::Unreachable { candidate, error } => (
            "unreachable",
            entry.endpoint.as_str(),
            Some(candidate),
            Some(error),
        ),
        QuicUpgrade::NotSaved { candidate, error } => (
            "not_saved",
            entry.endpoint.as_str(),
            Some(candidate),
            Some(error),
        ),
        QuicUpgrade::Upgraded { to, .. } => ("upgraded", to.as_str(), Some(to), None),
    };
    serde_json::json!({
        "name": entry.name,
        "status": status,
        "previous_endpoint": entry.endpoint,
        "endpoint": endpoint,
        "candidate": candidate,
        "error": error,
    })
}

/// One `host upgrade` result for a person.
pub(super) fn upgrade_line(entry: &RemoteEntry, outcome: &QuicUpgrade) -> String {
    let name = &entry.name;
    let endpoint = &entry.endpoint;
    match outcome {
        QuicUpgrade::Upgraded { from, to } => format!("{name}: upgraded {from} -> {to}"),
        QuicUpgrade::AlreadyQuic => format!("{name}: already on QUIC ({endpoint})"),
        QuicUpgrade::NoRoute => format!(
            "{name}: no QUIC route to try from {endpoint}; `phux host add {name}` over ssh \
             pairs one"
        ),
        QuicUpgrade::Unreachable { candidate, error } => format!(
            "{name}: staying on {endpoint}; QUIC at {candidate} failed ({error}). \
             Check UDP {} is open to the host and that its server binds QUIC \
             (`phux doctor` there)",
            candidate.rsplit(':').next().unwrap_or("8788")
        ),
        QuicUpgrade::NotSaved { candidate, error } => format!(
            "{name}: QUIC at {candidate} answers, but the entry could not be rewritten: {error}"
        ),
    }
}
