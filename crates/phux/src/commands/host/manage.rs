//! Looking a host up by name, and the verbs that edit or remove it:
//! `show`, `attach`, `rename`, `enable`/`disable`, `rm`.

use std::path::Path;
use std::process::ExitCode;

use toml_edit::{Item, value};

use super::super::json_err::{self, CliError, codes};
use super::super::remote::{self};
use super::super::satellite as satellite_registry;

use super::add::{registry_failure, reject_entry};
use super::hub::{HubReload, add_hub_reload, reload_local_hub};
use super::list::{HostRow, auth_display, row_json};
use super::{HostRole, inbound};

/// Decide which registry `host rm NAME` removes from. A name in both with no
/// `--role` is refused (exit 2): `rm` is destructive, so never guess.
pub(super) fn resolve_rm_role(
    name: &str,
    requested: Option<HostRole>,
    in_remote: bool,
    in_satellite: bool,
) -> Result<HostRole, (CliError, u8)> {
    resolve_host_role(name, requested, in_remote, in_satellite, "rm")
}

pub(super) fn resolve_host_role(
    name: &str,
    requested: Option<HostRole>,
    in_remote: bool,
    in_satellite: bool,
    verb: &str,
) -> Result<HostRole, (CliError, u8)> {
    let found = |role| match role {
        HostRole::Remote => in_remote,
        HostRole::Satellite => in_satellite,
    };
    match requested {
        Some(role) if found(role) => Ok(role),
        Some(role) if found(role.other()) => {
            let other = role.other().as_str();
            let remedy = if matches!(verb, "enable" | "disable") {
                "only satellites have an enabled state; remotes are always attachable".to_owned()
            } else {
                format!(
                    "it is registered as a {other}; run `phux host {verb} --role {other} {name}`"
                )
            };
            Err((
                CliError::new(
                    codes::REGISTRY,
                    format!("{name:?} is not registered as a {}", role.as_str()),
                    remedy,
                ),
                1,
            ))
        }
        None if in_remote && in_satellite => Err((
            CliError::new(
                codes::REGISTRY,
                format!("{name:?} is registered as both a remote and a satellite"),
                format!(
                    "name the registry to remove from: \
                      `phux host {verb} --role remote {name}` or \
                      `phux host {verb} --role satellite {name}`"
                ),
            ),
            2,
        )),
        None if in_remote => Ok(HostRole::Remote),
        None if in_satellite => Ok(HostRole::Satellite),
        _ => Err((
            CliError::new(
                codes::REGISTRY,
                format!("{name:?} is not registered"),
                "`phux host ls` lists every registered host",
            ),
            1,
        )),
    }
}

pub(super) fn find_host(
    name: &str,
    role: Option<HostRole>,
    verb: &str,
) -> Result<HostRow, (CliError, u8)> {
    let remotes = remote::load_registry().map_err(|err| (registry_failure(err), 1))?;
    let satellites =
        satellite_registry::load_registry().map_err(|err| (registry_failure(err), 1))?;
    let remote = remotes.into_iter().find(|entry| entry.name == name);
    let satellite = satellites.into_iter().find(|entry| entry.name == name);
    let resolved = resolve_host_role(name, role, remote.is_some(), satellite.is_some(), verb)?;
    match resolved {
        HostRole::Remote => remote.map(HostRow::from_remote),
        HostRole::Satellite => satellite.map(HostRow::from_satellite),
    }
    .ok_or_else(|| {
        (
            registry_failure("host disappeared during lookup".to_owned()),
            1,
        )
    })
}

pub(super) fn run_show(name: &str, role: Option<HostRole>, json: bool) -> ExitCode {
    let row = match find_host(name, role, "show") {
        Ok(row) => row,
        Err((err, code)) => return json_err::emit(json, &err, code),
    };
    if json {
        return crate::output::json(
            &serde_json::json!({"schema_version": 1, "host": row_json(&row)}),
        );
    }
    outln!("{} ({})", row.name, row.role.as_str());
    outln!("  Endpoint: {}", row.endpoint);
    if let Some(enabled) = row.enabled {
        outln!("  State: {}", if enabled { "enabled" } else { "disabled" });
    }
    outln!(
        "  Auth: {}",
        auth_display(
            &row.endpoint,
            row.token_file.is_some(),
            row.cert_fingerprint.is_some()
        )
    );
    if let Some(path) = row.token_file {
        outln!("  Token file: {}", path.display());
    }
    if let Some(fingerprint) = row.cert_fingerprint {
        outln!("  Certificate fingerprint: {fingerprint}");
    }
    if let Some(server_name) = row.tls_server_name {
        outln!("  TLS server name: {server_name}");
    }
    if let Some(session) = row.session {
        outln!("  Session: {session}");
    }
    if let Some(ssh) = row.ssh {
        outln!("  SSH: {ssh}");
    }
    if let Some(direct) = row.direct {
        outln!("  Direct route (tried first): {direct}");
    }
    if let Some(cert) = row.client_cert {
        outln!("  Client certificate: {}", cert.display());
    }
    ExitCode::SUCCESS
}

pub(super) fn run_attach(name: &str) -> ExitCode {
    let entries = match remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(false, &registry_failure(err), 1),
    };
    if let Some(entry) = entries.into_iter().find(|entry| entry.name == name) {
        return super::super::remote_target::run_registered(name, entry, None);
    }
    let satellites = match satellite_registry::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(false, &registry_failure(err), 1),
    };
    let remedy = if satellites.iter().any(|entry| entry.name == name) {
        "satellites are hub-dialed, not directly attachable; register a remote with `phux host add`"
    } else {
        "`phux host ls --role remote` lists attachable hosts"
    };
    json_err::emit(
        false,
        &CliError::new(
            codes::REGISTRY,
            format!("no remote host named {name:?}"),
            remedy,
        ),
        1,
    )
}

/// Change only the named field on the exact root entry, under the shared
/// registry lock. Inherited entries cannot be modified through this file.
pub(super) fn edit_host_field(
    row: &HostRow,
    field: &str,
    replacement: toml_edit::Value,
) -> Result<(), String> {
    let path = phux_config::loader::config_path();
    edit_host_field_at(&path, row, field, replacement)
}

pub(super) fn edit_host_field_at(
    path: &Path,
    row: &HostRow,
    field: &str,
    replacement: toml_edit::Value,
) -> Result<(), String> {
    let mut doc = super::super::toml_registry::edit_document(path)?;
    let key = match row.role {
        HostRole::Remote => "remote",
        HostRole::Satellite => "satellites",
    };
    let tables = super::super::toml_registry::tables_mut(&mut doc, key)?;
    let matches = super::super::toml_registry::machine_indices(tables, &row.name, &row.endpoint);
    let [index] = matches.as_slice() else {
        return Err("host is ambiguous or inherited; edit its source configuration".to_owned());
    };
    if field == "name"
        && tables
            .iter()
            .any(|table| table.get("name").and_then(Item::as_str) == replacement.as_str())
    {
        return Err("new host name is already registered in this role".to_owned());
    }
    let table = tables
        .get_mut(*index)
        .ok_or_else(|| "host disappeared during edit".to_owned())?;
    table.insert(field, Item::Value(replacement));
    // A direct remote without an explicit SSH address formerly fell back to
    // its label for repair. Preserve that destination when changing the label.
    if field == "name"
        && row.role == HostRole::Remote
        && row.ssh.is_none()
        && !row.endpoint.starts_with("ssh://")
    {
        table.insert("ssh", value(&row.name));
    }
    doc.commit()
}

pub(super) fn run_rename(
    name: &str,
    new_name: &str,
    role: Option<HostRole>,
    json: bool,
) -> ExitCode {
    let row = match find_host(name, role, "rename") {
        Ok(row) => row,
        Err((err, code)) => return json_err::emit(json, &err, code),
    };
    if let Some(err) = inbound_rename_refusal(&row) {
        return json_err::emit(json, &err, 2);
    }
    let validated = match row.role {
        HostRole::Remote => remote::validate_name(new_name),
        HostRole::Satellite => satellite_registry::registry_name(new_name),
    };
    let new_name = match validated {
        Ok(name) => name,
        Err(err) => return json_err::emit(json, &reject_entry(err), 2),
    };
    if new_name == row.name {
        return json_err::emit(
            json,
            &reject_entry("new name is the same as the current name".to_owned()),
            2,
        );
    }
    let exists = match row.role {
        HostRole::Remote => remote::load_registry()
            .map(|entries| entries.iter().any(|entry| entry.name == new_name)),
        HostRole::Satellite => satellite_registry::load_registry()
            .map(|entries| entries.iter().any(|entry| entry.name == new_name)),
    };
    match exists {
        Ok(true) => {
            return json_err::emit(
                json,
                &reject_entry(format!("{new_name:?} is already registered in this role")),
                2,
            );
        }
        Err(err) => return json_err::emit(json, &registry_failure(err), 1),
        Ok(false) => {}
    }
    if let Err(err) = edit_host_field(&row, "name", new_name.clone().into()) {
        return json_err::emit(json, &registry_failure(err), 1);
    }
    let reload = (row.role == HostRole::Satellite).then(reload_local_hub);
    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "renamed": {"from": name, "to": new_name, "role": row.role.as_str()},
            "requires_restart": reload.as_ref().is_some_and(HubReload::requires_restart),
        });
        add_hub_reload(&mut doc, reload.as_ref());
        return crate::output::json(&doc);
    }
    outln!("Renamed {} {name:?} to {new_name:?}.", row.role.as_str());
    if let Some(reload) = &reload {
        outln!("  {}", reload.describe());
    }
    ExitCode::SUCCESS
}

/// An `inbound://` satellite's credential names the satellite it was
/// enrolled as (ADR-0182), so renaming only the entry would strand the
/// joined machine under a name the hub no longer adopts.
pub(super) fn inbound_rename_refusal(row: &HostRow) -> Option<CliError> {
    let inbound =
        row.role == HostRole::Satellite && phux_server::hub::is_inbound_endpoint(&row.endpoint);
    inbound.then(|| {
        CliError::new(
            codes::USAGE_ERROR,
            format!(
                "{:?} is an inbound satellite; its credential is bound to that name",
                row.name
            ),
            "run `phux host rm NAME` (which revokes it), then `phux host ticket NEW` and \
             `phux host join` on that machine again",
        )
    })
}

pub(super) fn run_enabled(name: &str, enabled: bool, json: bool) -> ExitCode {
    let mut row = match find_host(
        name,
        Some(HostRole::Satellite),
        if enabled { "enable" } else { "disable" },
    ) {
        Ok(row) => row,
        Err((err, code)) => return json_err::emit(json, &err, code),
    };
    if let Err(err) = edit_host_field(&row, "enabled", enabled.into()) {
        return json_err::emit(json, &registry_failure(err), 1);
    }
    row.enabled = Some(enabled);
    let state = if enabled { "enabled" } else { "disabled" };
    let reload = reload_local_hub();
    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "host": row_json(&row),
            "requires_restart": reload.requires_restart(),
        });
        add_hub_reload(&mut doc, Some(&reload));
        return crate::output::json(&doc);
    }
    outln!("Satellite {name:?} {state} in config.");
    outln!("  {}", reload.describe());
    ExitCode::SUCCESS
}

/// `phux host rm`.
pub(super) fn run_remove(name: &str, role: Option<HostRole>, json: bool) -> ExitCode {
    let remotes = match remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(json, &registry_failure(err), 1),
    };
    let satellites = match satellite_registry::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(json, &registry_failure(err), 1),
    };
    let remote_entry = remotes.into_iter().find(|entry| entry.name == name);
    let satellite_entry = satellites.into_iter().find(|entry| entry.name == name);

    let resolved = match resolve_rm_role(
        name,
        role,
        remote_entry.is_some(),
        satellite_entry.is_some(),
    ) {
        Ok(resolved) => resolved,
        Err((err, exit_code)) => return json_err::emit(json, &err, exit_code),
    };

    let (removed, token_file) = match resolved {
        HostRole::Remote => {
            // The arm is only reachable when the entry was found.
            let Some(entry) = remote_entry else {
                return json_err::emit(json, &registry_failure("remote entry vanished".into()), 1);
            };
            (remote::remove_entry(&entry), entry.token_file)
        }
        HostRole::Satellite => {
            let Some(entry) = satellite_entry else {
                return json_err::emit(
                    json,
                    &registry_failure("satellite entry vanished".into()),
                    1,
                );
            };
            (satellite_registry::remove_entry(&entry), entry.token_file)
        }
    };
    if let Err(err) = removed {
        return json_err::emit(json, &registry_failure(err), 1);
    }
    // A satellite that dialed in holds a credential this hub issued; removing
    // it revokes that credential too (ADR-0182). A registry that cannot be
    // written is reported and does not undo the removal.
    let revoked = if resolved == HostRole::Satellite {
        match inbound::revoke_satellite(name) {
            Ok(count) => Some(count),
            Err(err) => {
                warn!(
                    "could not revoke {name:?}'s satellite credential ({err}); revoke it with \
                     `phux workload revoke`"
                );
                None
            }
        }
    } else {
        None
    };
    let reload = (resolved == HostRole::Satellite).then(reload_local_hub);

    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "removed": { "name": name, "role": resolved.as_str() },
        });
        add_hub_reload(&mut doc, reload.as_ref());
        if let Some(revoked) = revoked {
            doc["revoked_credentials"] = serde_json::json!(revoked);
        }
        return crate::output::json(&doc);
    }
    outln!("Removed {} {name:?}.", resolved.as_str());
    if let Some(reload) = &reload {
        outln!("  {}", reload.describe());
    }
    if let Some(revoked) = revoked.filter(|revoked| *revoked > 0) {
        outln!("  revoked its satellite credential ({revoked})");
    }
    if let Some(path) = token_file {
        outln!("Its token file is still at {}.", path.display());
    }
    ExitCode::SUCCESS
}
