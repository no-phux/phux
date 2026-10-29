//! The authority and registry verbs of `phux workload`: `authority
//! [--init|--rotate]`, `list`, `repair-paired`, and `revoke`.

use std::path::PathBuf;

use chrono::Utc;
use phux_server::workload::{self, WorkloadCredential, WorkloadPaths, WorkloadRegistry};
use serde_json::json;

use super::{Report, cli_error, timestamp};
use crate::commands::json_err::CliError;
use crate::commands::update::apply::hex_lower;

pub(super) fn authority(paths: &WorkloadPaths, init: bool) -> Result<Report, CliError> {
    let status = if init {
        workload::init_authority(&paths.ca_cert, &paths.ca_key)
    } else {
        workload::ca_fingerprint(&paths.ca_cert).map(|fingerprint| workload::AuthorityStatus {
            fingerprint,
            created: false,
        })
    }
    .map_err(|error| cli_error(&error))?;
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "ca_fingerprint": status.fingerprint,
            "created": status.created,
        }),
        prose: vec![status.fingerprint],
        payload: None,
    })
}

/// `phux workload authority --rotate` (ADR-0153).
pub(super) fn rotate(paths: &WorkloadPaths) -> Result<Report, CliError> {
    let (cert, key) = server_pair_paths();
    let server = cert
        .as_deref()
        .zip(key.as_deref())
        .map(|(cert, key)| workload::ServerPair { cert, key });
    let rotation = workload::rotate_authority(paths, server).map_err(|error| cli_error(&error))?;
    let mut prose = vec![
        "Rotated the workload certificate authority.".to_owned(),
        format!("  previous  {}", rotation.previous),
        format!("  new       {}", rotation.fingerprint),
    ];
    match &rotation.server_leaf {
        Some(leaf) => prose.push(format!("  server certificate re-issued under it: {leaf}")),
        None if cert.is_none() => prose.push(
            "  server certificate left alone: PHUX_WS_TLS_CERT/PHUX_WS_TLS_KEY name an operator-supplied one"
                .to_owned(),
        ),
        None => prose.push(
            "  no server certificate yet; the first remote listener is issued one under the new authority"
                .to_owned(),
        ),
    }
    prose.extend([
        format!(
            "The replaced files are kept beside the new ones with the suffix .{}.",
            rotation.retired_suffix
        ),
        "Next:".to_owned(),
        "  1. Restart the server to present it: `phux upgrade` (sessions survive).".to_owned(),
        "  2. Re-pair every client. Each refuses this server, naming both fingerprints, until it does:"
            .to_owned(),
        "       `phux host add NAME` on each machine that added this host over ssh (it re-enrolls too)"
            .to_owned(),
        "       `phux pair --qr` here, scanned by each phone".to_owned(),
        "     Workload credentials the old authority issued no longer verify; `phux host add` re-enrolls them."
            .to_owned(),
    ]);
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "rotated": true,
            "ca_fingerprint": rotation.fingerprint,
            "previous_ca_fingerprint": rotation.previous,
            "server_cert_fingerprint": rotation.server_leaf,
            "retired_suffix": rotation.retired_suffix,
        }),
        prose,
        payload: None,
    })
}

/// The server's own TLS pair, as the server resolves it: `None` for both when
/// `PHUX_WS_TLS_CERT` or `PHUX_WS_TLS_KEY` names an operator-supplied one,
/// which rotation never touches.
fn server_pair_paths() -> (Option<PathBuf>, Option<PathBuf>) {
    if std::env::var_os("PHUX_WS_TLS_CERT").is_some()
        || std::env::var_os("PHUX_WS_TLS_KEY").is_some()
    {
        return (None, None);
    }
    (
        Some(phux_server::transport::tls::default_cert_path()),
        Some(phux_server::transport::tls::default_key_path()),
    )
}

pub(super) fn list(paths: &WorkloadPaths, public_keys: bool) -> Result<Report, CliError> {
    let registry = WorkloadRegistry::load(&paths.registry).map_err(|error| cli_error(&error))?;
    let now = Utc::now().timestamp();
    let credentials = registry.credentials();
    let mut prose = vec![format!(
        "registry generation {}, {} credential(s)",
        registry.generation(),
        credentials.len()
    )];
    for credential in credentials {
        prose.extend(credential_lines(credential, now, public_keys));
    }
    let documents: Vec<serde_json::Value> = credentials
        .iter()
        .map(|credential| credential_document(credential, now, public_keys))
        .collect();
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "registry_instance": registry.instance_id(),
            "registry_generation": registry.generation(),
            "credentials": documents,
        }),
        prose,
        payload: None,
    })
}

fn status(credential: &WorkloadCredential, now: i64) -> &'static str {
    if credential.revoked_at.is_some() {
        "revoked"
    } else if credential.is_active_at(now) {
        "active"
    } else {
        "expired"
    }
}

fn credential_lines(credential: &WorkloadCredential, now: i64, public_keys: bool) -> Vec<String> {
    let expires = credential
        .expires_at
        .map_or_else(|| "never".to_owned(), timestamp);
    let mut lines = vec![
        format!("{}  {}", credential.id, status(credential, now)),
        credential.satellite.as_ref().map_or_else(
            || format!("  scopes:  {}", credential.scopes.join(" ")),
            |name| format!("  satellite: {name} (no terminal scope)"),
        ),
        format!("  principal: {}", credential.enrolled_for().as_str()),
        format!("  expires: {expires}"),
    ];
    if let Some(revoked_at) = credential.revoked_at {
        lines.push(format!("  revoked: {}", timestamp(revoked_at)));
    }
    if public_keys {
        lines.push(format!(
            "  public key: {}",
            hex_lower(&credential.public_key)
        ));
    }
    lines
}

fn credential_document(
    credential: &WorkloadCredential,
    now: i64,
    public_keys: bool,
) -> serde_json::Value {
    let mut document = json!({
        "credential_id": credential.id,
        "status": status(credential, now),
        "scopes": credential.scopes,
        "principal": credential.enrolled_for().as_str(),
        "expires_at": credential.expires_at,
        "revoked_at": credential.revoked_at,
    });
    if public_keys {
        document["public_key"] = json!(hex_lower(&credential.public_key));
    }
    if let Some(name) = &credential.satellite {
        document["satellite"] = json!(name);
    }
    document
}

pub(super) fn repair_paired(
    paths: &WorkloadPaths,
    credential_id: &str,
) -> Result<Report, CliError> {
    let outcome = WorkloadRegistry::repair_paired_device(&paths.registry, credential_id)
        .map_err(|error| cli_error(&error))?;
    let status = if outcome.updated {
        "Repaired"
    } else {
        "Already repaired"
    };
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "credential_id": credential_id,
            "registry_generation": outcome.generation,
            "updated": outcome.updated,
            "scopes": [workload::PAIRED_DEVICE_SCOPE],
        }),
        prose: vec![
            format!("{status}: {credential_id} (registry generation {}).", outcome.generation),
            "Reconnect the device to use full control. Its key and expiry are unchanged; no re-pair or server restart is needed.".to_owned(),
        ],
        payload: None,
    })
}

pub(super) fn revoke(paths: &WorkloadPaths, credential_id: &str) -> Result<Report, CliError> {
    if workload::tickets::is_ticket_id(credential_id) {
        return revoke_ticket(paths, credential_id);
    }
    let outcome = WorkloadRegistry::revoke(&paths.registry, credential_id)
        .map_err(|error| cli_error(&error))?;
    let prose = if outcome.newly_revoked {
        vec![
            format!(
                "Revoked {credential_id} (registry generation {}).",
                outcome.generation
            ),
            "A running server refuses it from its next connection attempt and ends its live connections; no restart is needed."
                .to_owned(),
        ]
    } else {
        vec![format!(
            "{credential_id} was already revoked at {}.",
            timestamp(outcome.revoked_at)
        )]
    };
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "operation": "revoke",
            "credential_id": credential_id,
            "revoked_at": outcome.revoked_at,
            "newly_revoked": outcome.newly_revoked,
            "registry_generation": outcome.generation,
        }),
        prose,
        payload: None,
    })
}

/// Withdraw an unspent enrollment ticket (`t-…`): it is refused from now on,
/// like a consumed one (ADR-0154, ADR-0182).
fn revoke_ticket(paths: &WorkloadPaths, ticket_id: &str) -> Result<Report, CliError> {
    let newly_revoked = workload::tickets::revoke_ticket(
        &workload::tickets::tickets_path(&paths.registry),
        ticket_id,
    )
    .map_err(|error| cli_error(&error))?;
    let prose = vec![if newly_revoked {
        format!("Revoked enrollment ticket {ticket_id}; nothing can enroll with it now.")
    } else {
        format!("Enrollment ticket {ticket_id} was already used, expired, or revoked.")
    }];
    Ok(Report {
        document: json!({
            "schema_version": 1,
            "operation": "revoke_ticket",
            "ticket_id": ticket_id,
            "newly_revoked": newly_revoked,
        }),
        prose,
        payload: None,
    })
}
