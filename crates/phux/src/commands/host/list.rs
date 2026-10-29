//! The merged host row and `phux host ls` rendering (table and JSON).

use std::path::PathBuf;
use std::process::ExitCode;

use super::super::json_err::{self};
use super::super::remote::{self};
use super::super::satellite as satellite_registry;

use super::HostRole;
use super::add::registry_failure;

/// One merged view of an entry from either registry, shaped for the table
/// and the JSON document.
#[derive(Debug)]
pub(super) struct HostRow {
    pub(super) name: String,
    pub(super) role: HostRole,
    pub(super) endpoint: String,
    /// `Some` for satellites; `None` for remotes, whose schema has no
    /// enabled bit.
    pub(super) enabled: Option<bool>,
    pub(super) token_file: Option<PathBuf>,
    pub(super) cert_fingerprint: Option<String>,
    /// Remote only: the TLS server name dials offer (a relay route).
    pub(super) tls_server_name: Option<String>,
    /// `Some` only for remotes; a hub-dialed satellite link has no arrival
    /// to attach.
    pub(super) session: Option<String>,
    /// Remote only: the ssh destination the entry was enrolled through.
    pub(super) ssh: Option<String>,
    /// Remote only: a paired direct route kept beside an `ssh://` or `wss://`
    /// endpoint.
    pub(super) direct: Option<String>,
    /// Remote only: the enrolled workload client certificate chain.
    pub(super) client_cert: Option<PathBuf>,
    /// Remote only: its private key's path (never its bytes).
    pub(super) client_key: Option<PathBuf>,
}

impl HostRow {
    pub(super) fn from_remote(entry: remote::RemoteEntry) -> Self {
        Self {
            name: entry.name,
            role: HostRole::Remote,
            endpoint: entry.endpoint,
            enabled: None,
            token_file: entry.token_file,
            cert_fingerprint: entry.cert_fingerprint,
            tls_server_name: entry.tls_server_name,
            session: entry.session,
            ssh: entry.ssh,
            direct: entry.direct,
            client_cert: entry.client_cert,
            client_key: entry.client_key,
        }
    }

    pub(super) fn from_new_remote(new: remote::NewRemote) -> Self {
        Self {
            name: new.name,
            role: HostRole::Remote,
            endpoint: new.endpoint,
            enabled: None,
            token_file: new.token_file,
            cert_fingerprint: new.cert_fingerprint,
            tls_server_name: new.tls_server_name,
            session: new.session,
            ssh: new.ssh,
            direct: new.direct,
            client_cert: new.client_cert,
            client_key: new.client_key,
        }
    }

    pub(super) fn from_satellite(entry: satellite_registry::SatelliteEntry) -> Self {
        Self {
            name: entry.name,
            role: HostRole::Satellite,
            endpoint: entry.endpoint,
            enabled: Some(entry.enabled),
            token_file: entry.token_file,
            cert_fingerprint: entry.cert_fingerprint,
            tls_server_name: None,
            session: None,
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        }
    }
}

/// `phux host ls`.
pub(super) fn run_list(role: Option<HostRole>, json: bool) -> ExitCode {
    let mut rows = Vec::new();
    if role != Some(HostRole::Satellite) {
        match remote::load_registry() {
            Ok(entries) => rows.extend(entries.into_iter().map(HostRow::from_remote)),
            Err(err) => return json_err::emit(json, &registry_failure(err), 1),
        }
    }
    if role != Some(HostRole::Remote) {
        match satellite_registry::load_registry() {
            Ok(entries) => rows.extend(entries.into_iter().map(HostRow::from_satellite)),
            Err(err) => return json_err::emit(json, &registry_failure(err), 1),
        }
    }
    sort_rows(&mut rows);

    if json {
        let hosts: Vec<_> = rows.iter().map(row_json).collect();
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "hosts": hosts,
        }));
    }
    if rows.is_empty() {
        outln!("{}", empty_state(role));
        return ExitCode::SUCCESS;
    }
    out!("{}", render_table(&rows));
    ExitCode::SUCCESS
}

/// Merged ordering: by name, remotes before satellites on a name shared by
/// both registries — stable regardless of config-file entry order, so the
/// table (and diffs of it) do not reshuffle when an entry is re-added.
pub(super) fn sort_rows(rows: &mut [HostRow]) {
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.role.cmp(&b.role)));
}

/// The teaching empty state: name the command that fills the table, per
/// role when the listing was filtered.
pub(super) const fn empty_state(role: Option<HostRole>) -> &'static str {
    match role {
        None => {
            "No hosts registered.\n\
             Add a machine you can ssh to with `phux host add me@HOST`; it is set up end to end.\n\
             Add a satellite this hub dials with `phux host add --role satellite me@HOST`."
        }
        Some(HostRole::Remote) => {
            "No remote hosts registered.\n\
             Add a machine you can ssh to with `phux host add me@HOST`; it is set up end to end."
        }
        Some(HostRole::Satellite) => {
            "No satellite hosts registered.\n\
             Add one with `phux host add --role satellite me@HOST`."
        }
    }
}

/// How the link authenticates, classified from the endpoint's transport and
/// the entry's auth material. `ssh://` rides the operator's existing ssh
/// trust; the paired transports report which halves of the token + pin pair
/// are on file.
pub(super) fn auth_display(endpoint: &str, has_token: bool, has_pin: bool) -> &'static str {
    if endpoint.trim().starts_with("ssh://") {
        return "ssh";
    }
    if phux_server::hub::is_inbound_endpoint(endpoint) {
        // The satellite dials in with the credential its ticket enrolled.
        return "cert";
    }
    match (has_token, has_pin) {
        (true, true) => "token+pin",
        (false, true) => "pin",
        (true, false) => "token",
        (false, false) => "none",
    }
}

/// The human table: NAME / ROLE / ENDPOINT / STATE / AUTH, one header line,
/// one row per entry.
pub(super) fn render_table(rows: &[HostRow]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<16} {:<10} {:<40} {:<9} AUTH",
        "NAME", "ROLE", "ENDPOINT", "STATE"
    );
    for row in rows {
        let state = match row.enabled {
            None => "-",
            Some(true) => "enabled",
            Some(false) => "disabled",
        };
        let auth = auth_display(
            &row.endpoint,
            row.token_file.is_some(),
            row.cert_fingerprint.is_some(),
        );
        let _ = writeln!(
            out,
            "{:<16} {:<10} {:<40} {state:<9} {auth}",
            row.name,
            row.role.as_str(),
            row.endpoint
        );
    }
    out
}

pub(super) fn row_json(row: &HostRow) -> serde_json::Value {
    serde_json::json!({
        "name": row.name,
        "role": row.role.as_str(),
        "endpoint": row.endpoint,
        "enabled": row.enabled,
        // Auth material by reference only: the token-file *path* is
        // machine-readable, the token bytes behind it never appear.
        "token_file": row.token_file.as_ref().map(|p| p.display().to_string()),
        "cert_fingerprint": row.cert_fingerprint,
        "tls_server_name": row.tls_server_name,
        "session": row.session,
        "ssh": row.ssh,
        "direct": row.direct,
        // Paths only, like the token file: the key bytes never appear.
        "client_cert": row.client_cert.as_ref().map(|p| p.display().to_string()),
        "client_key": row.client_key.as_ref().map(|p| p.display().to_string()),
    })
}
