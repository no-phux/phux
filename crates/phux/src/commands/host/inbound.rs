//! Reverse enrollment (ADR-0182): `phux host ticket NAME` on a hub, and
//! `phux host join CODE` on a machine the hub cannot dial.
//!
//! `ticket` mints a single-use satellite ticket into the workload store,
//! registers `[[satellites]] NAME` as `inbound://`, and prints the join code.
//! `join` enrolls a key it generates with that code, records `[[uplink]]`,
//! and rings the local server, which dials the hub at once.
//!
//! With `--mail-only` on both, `ticket` registers no entry and `join` records
//! `mail-only = true`, which the server never dials: the joined machine's
//! `phux mail` and `phux claim` use the hub's store, and the hub reaches
//! none of its terminals.
//!
//! `host ticket --json` emits `{"schema_version":1,"satellite","ticket_id",
//! "expires_at","hub","ca_fingerprint","join_code","enabled","hub_reload"}`; `host join
//! --json` emits `{"schema_version":1,"satellite","hub","credential_id",
//! "client_cert","client_key","server_reload"}`. Both follow the shared JSON
//! error contract on failure.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::loader as config_loader;
use phux_server::uplink::{JoinCode, Joined};
use phux_server::workload::{WorkloadPaths, tickets};
use toml_edit::{Table, value};

use super::super::json_err::{self, CliError, codes};
use super::super::satellite as satellite_registry;
use super::super::toml_registry;
use super::add::registry_failure;
use super::hub::{HubReload, add_hub_reload, reload_local_hub};

/// The `config.toml` key the uplink registry owns.
const UPLINK_KEY: &str = "uplink";

/// `phux host ticket NAME`.
pub(super) fn run_ticket(
    name: &str,
    hub: Option<&str>,
    minutes: i64,
    mail_only: bool,
    json: bool,
) -> ExitCode {
    match mint(name, hub, minutes, mail_only) {
        Ok(minted) => report_ticket(&minted, json),
        Err(err) => json_err::emit(json, &err, 1),
    }
}

/// The config, workload store, and server certificate a mint or revocation
/// uses. Tests pass a scratch copy; the process default is [`HubFiles::from_env`].
#[derive(Debug, Clone)]
pub(super) struct HubFiles {
    pub(super) config: PathBuf,
    pub(super) workload: WorkloadPaths,
    pub(super) cert: PathBuf,
}

impl HubFiles {
    pub(super) fn from_env() -> Self {
        let cert = std::env::var_os("PHUX_WS_TLS_CERT").map_or_else(
            phux_server::transport::tls::default_cert_path,
            PathBuf::from,
        );
        Self {
            config: config_loader::config_path(),
            workload: WorkloadPaths::from_env(),
            cert,
        }
    }
}

/// Mint NAME's join code for a satellite that dials `hub`, as `phux host
/// ticket` does, for a caller that delivers it itself (`phux host
/// minimal`): the code and the ticket id that withdraws it.
///
/// `adopt_inbound` updates an existing `inbound://` entry. `phux host
/// minimal` sets it only for a name it minted; anything else is refused so
/// a box cannot take over a satellite this feature did not create.
pub(super) fn mint_code(
    name: &str,
    hub: &str,
    minutes: i64,
    files: &HubFiles,
    adopt_inbound: bool,
) -> Result<(String, String), CliError> {
    let minted = mint_in(name, Some(hub), minutes, files, adopt_inbound, false)?;
    Ok((minted.code.to_link(), minted.ticket.id))
}

/// Withdraw an unredeemed ticket `mint_code` returned.
pub(super) fn withdraw_ticket(registry: &Path, id: &str) -> Result<(), String> {
    let tickets_path = tickets::tickets_path(registry);
    tickets::revoke_ticket(&tickets_path, id)
        .map(drop)
        .map_err(|err| err.to_string())
}

/// A minted ticket and where it was registered.
struct Minted {
    name: String,
    ticket: tickets::MintedTicket,
    code: JoinCode,
    /// The entry's enabled state, kept from an existing `inbound://` entry.
    enabled: bool,
    /// No entry was registered: the code joins for mail and claims only.
    mail_only: bool,
    reload: HubReload,
}

/// Validate everything, mint the ticket, then register the entry: a bad
/// argument or a conflicting entry leaves the store and `config.toml`
/// untouched.
fn mint(name: &str, hub: Option<&str>, minutes: i64, mail_only: bool) -> Result<Minted, CliError> {
    mint_in(name, hub, minutes, &HubFiles::from_env(), true, mail_only)
}

fn mint_in(
    name: &str,
    hub: Option<&str>,
    minutes: i64,
    files: &HubFiles,
    adopt_inbound: bool,
    mail_only: bool,
) -> Result<Minted, CliError> {
    let name = satellite_registry::registry_name(name).map_err(reject)?;
    phux_server::workload::validate_satellite_name(&name).map_err(|err| reject(err.to_string()))?;
    require_paired(&files.config)?;
    let hub = match hub {
        Some(hub) => hub.to_owned(),
        None => live_hub_endpoint()?,
    };
    phux_server::uplink::validate_hub_endpoint(&hub).map_err(|err| {
        CliError::new(
            codes::USAGE_ERROR,
            format!("the hub endpoint cannot go in a join code: {err}"),
            "pass --hub HOST:PORT, the QUIC address the satellite dials, with no scheme",
        )
    })?;
    let ca = pinned_authority(&files.cert)?;
    let enabled = inbound_entry_enabled(&files.config, &name, adopt_inbound)?;
    let entry = satellite_registry::NewSatellite::new(
        &name,
        phux_server::hub::INBOUND_ENDPOINT,
        enabled,
        None,
        None,
    )
    .map_err(reject)?;
    let tickets_path = tickets::tickets_path(&files.workload.registry);
    let ticket = tickets::mint_satellite_ticket(
        &tickets_path,
        &name,
        tickets::DEFAULT_CREDENTIAL_SECONDS,
        minutes.saturating_mul(60),
    )
    .map_err(|err| {
        CliError::new(
            codes::WORKLOAD,
            format!("could not mint a satellite ticket: {err}"),
            "check the state directory's permissions, then rerun",
        )
    })?;
    let registered = if mail_only {
        Ok(())
    } else {
        satellite_registry::add_or_update_at(&files.config, &entry).map(drop)
    };
    if let Err(err) = registered {
        // Nothing points at the ticket yet; withdraw it rather than leave a
        // live secret for an entry that was never written.
        if let Err(revoke) = tickets::revoke_ticket(&tickets_path, &ticket.id) {
            warn!(
                "could not withdraw ticket {} ({revoke}); revoke it with `phux workload revoke {}`",
                ticket.id, ticket.id
            );
        }
        return Err(registry_failure(err));
    }
    let code = JoinCode {
        hub,
        server_name: None,
        ca,
        name: name.clone(),
        ticket: ticket.secret_hex.clone(),
    };
    Ok(Minted {
        name,
        ticket,
        code,
        enabled,
        mail_only,
        reload: reload_local_hub(),
    })
}

/// Whether NAME's `inbound://` entry is enabled: `true` for a new name, the
/// existing state for an `inbound://` entry when `adopt_inbound` is set. A
/// name registered under any other endpoint is refused, so a ticket never
/// silently replaces a dialed satellite or drops its auth material. An
/// `inbound://` name is refused the same way when the caller did not mint
/// it (`phux host minimal`).
fn inbound_entry_enabled(config: &Path, name: &str, adopt_inbound: bool) -> Result<bool, CliError> {
    let existing = satellite_registry::load_registry_from(config)
        .map_err(registry_failure)?
        .into_iter()
        .find(|entry| entry.name == name);
    match existing {
        None => Ok(true),
        Some(entry) if phux_server::hub::is_inbound_endpoint(&entry.endpoint) && adopt_inbound => {
            Ok(entry.enabled)
        }
        Some(entry) if phux_server::hub::is_inbound_endpoint(&entry.endpoint) => {
            Err(CliError::new(
                codes::REGISTRY,
                format!(
                    "satellite {name:?} is already registered as inbound://, and this mint will \
                     not adopt it"
                ),
                "pick another name",
            ))
        }
        Some(entry) => Err(CliError::new(
            codes::REGISTRY,
            format!(
                "satellite {name:?} is already registered at {}, which this hub dials",
                entry.endpoint
            ),
            "pick another name, or run `phux host rm NAME` first to replace it with an \
             inbound satellite",
        )),
    }
}

/// The workload CA fingerprint `cert` chains to.
fn pinned_authority(cert: &Path) -> Result<String, CliError> {
    phux_server::transport::tls::presented_authority(cert)
        .ok()
        .flatten()
        .ok_or_else(|| {
            CliError::new(
                codes::WORKLOAD,
                "this server's certificate was not issued by its workload CA, so a satellite has \
                 nothing to pin",
                "run `phux workload authority --rotate`, restart the server, and mint again",
            )
        })
}

/// Only a hub that verifies workload certificates offers the satellite ALPN
/// (ADR-0182), so a ticket minted for any other would enroll a satellite the
/// hub then refuses.
fn require_paired(config: &Path) -> Result<(), CliError> {
    let config =
        config_loader::load_from(config).map_err(|err| registry_failure(err.to_string()))?;
    if config.policy.mode == Some(phux_config::PolicyMode::Paired) {
        return Ok(());
    }
    Err(CliError::new(
        codes::WORKLOAD,
        "a hub adopts satellites only under `[policy] mode = \"paired\"`",
        "set `[policy] mode = \"paired\"` in config.toml, restart the server, and mint again",
    ))
}

fn live_hub_endpoint() -> Result<String, CliError> {
    let socket = phux_server::runtime::default_socket_path();
    let no_route = |detail: String| {
        CliError::new(
            codes::NO_SERVER,
            detail,
            "start the hub with a routable --quic listener, or pass --hub HOST:PORT naming \
             the address the satellite dials",
        )
    };
    super::super::pair::advertised_quic_endpoint(&socket)
        .map_err(no_route)?
        .ok_or_else(|| {
            no_route("this server has no QUIC listener another machine can dial".to_owned())
        })
}

fn reject(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "a satellite name is 1 to 255 characters with no '/', ':', or control character",
    )
}

fn report_ticket(minted: &Minted, json: bool) -> ExitCode {
    let link = minted.code.to_link();
    let expires = rfc3339(minted.ticket.expires_at);
    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "satellite": minted.name,
            "ticket_id": minted.ticket.id,
            "expires_at": expires,
            "hub": minted.code.hub,
            "ca_fingerprint": minted.code.ca,
            "join_code": link,
            "enabled": minted.enabled,
            "mail_only": minted.mail_only,
        });
        add_hub_reload(&mut doc, Some(&minted.reload));
        return crate::output::json(&doc);
    }
    if minted.mail_only {
        outln!(
            "Mail-only satellite {:?}: ticket {} (single use) expires {expires}. It reaches \
             this hub's mail and claims, and this hub reaches none of its terminals.",
            minted.name,
            minted.ticket.id
        );
    } else {
        outln!(
            "Satellite {:?} registered as inbound://; ticket {} (single use) expires {expires}.",
            minted.name,
            minted.ticket.id
        );
        outln!("  {}", minted.reload.describe());
    }
    if !minted.enabled {
        outln!(
            "  {:?} is disabled here; run `phux host enable {}` before it joins.",
            minted.name,
            minted.name
        );
    }
    outln!();
    outln!(
        "On the satellite, run `phux host join{} -` and paste this code:",
        if minted.mail_only { " --mail-only" } else { "" }
    );
    outln!("  {link}");
    outln!(
        "(Passing the code as an argument also works, but leaves it in `ps` and shell history.)"
    );
    outln!();
    outln!(
        "The code is a secret until it is used. Withdraw it with `phux workload revoke {}`.",
        minted.ticket.id
    );
    ExitCode::SUCCESS
}

/// Revoke every satellite credential this hub issued to `name`.
///
/// # Errors
///
/// The workload registry could not be read or written. The error is the
/// caller's to surface: `phux host minimal` keeps the box and retries, and
/// does not report the satellite revoked.
pub(super) fn revoke_satellite(name: &str) -> Result<usize, String> {
    revoke_satellite_at(&WorkloadPaths::from_env().registry, name)
}

pub(super) fn revoke_satellite_at(registry: &Path, name: &str) -> Result<usize, String> {
    phux_server::workload::WorkloadRegistry::revoke_satellite(registry, name)
        .map_err(|err| err.to_string())
}

/// `phux host join CODE`, or `phux host join -` to read CODE from stdin.
pub(super) fn run_join(code: &str, mail_only: bool, json: bool) -> ExitCode {
    let code = match read_code(code).and_then(|code| JoinCode::parse(&code)) {
        Ok(code) => code,
        Err(err) => {
            let err = CliError::new(
                codes::USAGE_ERROR,
                err,
                "paste the whole code `phux host ticket NAME` printed on the hub",
            );
            return json_err::emit(json, &err, 2);
        }
    };
    let dir = phux_server::telemetry::state_dir().join("uplinks");
    let joined = match super::super::cli_runtime() {
        Ok(rt) => rt.block_on(phux_server::uplink::join(&code, &dir)),
        Err(exit) => return exit,
    };
    let mut joined = match joined {
        Ok(joined) => joined,
        Err(err) => {
            let err = CliError::new(
                codes::WORKLOAD,
                format!("could not join the hub: {err}"),
                "mint a fresh code with `phux host ticket NAME` on the hub; a code works once \
                 and only for a few minutes",
            );
            return json_err::emit(json, &err, 1);
        }
    };
    joined.entry.mail_only = mail_only;
    if let Err(err) = record_uplink(&joined, &dir) {
        return json_err::emit(json, &unrecorded_uplink(&joined, &err), 1);
    }
    report_joined(&joined, &reload_local_hub(), json)
}

/// The code argument, or one line of stdin for `-` so the single-use
/// secret stays out of `ps` and shell history.
fn read_code(arg: &str) -> Result<String, String> {
    use std::io::{BufRead as _, IsTerminal as _};
    if arg != "-" {
        return Ok(arg.to_owned());
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        info!("Paste the join code from `phux host ticket`:");
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|err| format!("could not read the join code from stdin: {err}"))?;
    Ok(line.trim().to_owned())
}

/// The ticket is spent and the hub holds the credential, so a failed
/// `config.toml` write names the stored identity and the entry to add by
/// hand instead of leaving the files with nothing pointing at them.
fn unrecorded_uplink(joined: &Joined, err: &str) -> CliError {
    let entry = &joined.entry;
    let server_name = entry
        .server_name
        .as_ref()
        .map(|name| format!("server-name = {name:?}\n"))
        .unwrap_or_default();
    CliError::new(
        codes::REGISTRY,
        format!(
            "enrolled as credential {}, but could not record the uplink in config.toml: {err}",
            joined.credential
        ),
        format!(
            "add this entry to config.toml and run `phux config reload`:\n[[uplink]]\n\
             name = {:?}\nhub = {:?}\n{server_name}ca-fingerprint = {:?}\n\
             client-cert = {:?}\nclient-key = {:?}\nenabled = true",
            entry.name,
            entry.hub,
            entry.ca_fingerprint,
            entry.client_cert.display().to_string(),
            entry.client_key.display().to_string(),
        ),
    )
}

/// Write `joined`'s `[[uplink]]` entry, replacing one for the same name and
/// hub, and remove the identity files that entry named when this command
/// made them (the hub revoked that credential when it enrolled the new one).
fn record_uplink(joined: &Joined, dir: &Path) -> Result<(), String> {
    let entry = &joined.entry;
    let configured = config_loader::load().map_err(|err| err.to_string())?.uplink;
    let previous = configured
        .iter()
        .position(|old| old.name == entry.name && old.hub == entry.hub);
    let replaced = previous.and_then(|index| configured.get(index).cloned());
    toml_registry::upsert_machine(
        &config_loader::config_path(),
        UPLINK_KEY,
        || Ok(previous),
        |table| fill_uplink(table, entry),
    )?;
    if let Some(old) = replaced.filter(|old| old.client_key != entry.client_key) {
        for path in [&old.client_key, &old.client_cert] {
            if path.starts_with(dir) {
                phux_server::workload::remove_owned_file(path);
            }
        }
    }
    Ok(())
}

fn fill_uplink(table: &mut Table, entry: &phux_config::UplinkConfigEntry) {
    table.insert("name", value(&entry.name));
    table.insert("hub", value(&entry.hub));
    toml_registry::set_or_remove(table, "server-name", entry.server_name.as_ref());
    table.insert("ca-fingerprint", value(&entry.ca_fingerprint));
    table.insert(
        "client-cert",
        value(entry.client_cert.display().to_string()),
    );
    table.insert("client-key", value(entry.client_key.display().to_string()));
    table.insert("enabled", value(entry.enabled));
    if entry.mail_only {
        table.insert("mail-only", value(true));
    } else {
        table.remove("mail-only");
    }
}

fn report_joined(joined: &Joined, reload: &HubReload, json: bool) -> ExitCode {
    let entry = &joined.entry;
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "satellite": entry.name,
            "hub": entry.hub,
            "credential_id": joined.credential,
            "client_cert": entry.client_cert,
            "client_key": entry.client_key,
            "mail_only": entry.mail_only,
            "server_reload": reload.as_json_str(),
        });
        return crate::output::json(&doc);
    }
    outln!(
        "Joined the hub at {} as satellite {:?} (credential {}).",
        entry.hub,
        entry.name,
        joined.credential
    );
    outln!("  identity: {}", entry.client_cert.display());
    if entry.mail_only {
        outln!(
            "  mail only: `phux mail` and `phux claim` here use the hub's store; the hub \
             reaches no terminal here"
        );
        return ExitCode::SUCCESS;
    }
    outln!(
        "  {}",
        match reload {
            HubReload::Signalled => "the running server is dialing the hub now".to_owned(),
            HubReload::NoServer => "no server is running here; start one (`phux server`, or \
                 `phux service install`) and it dials the hub"
                .to_owned(),
            HubReload::Failed(reason) => format!(
                "could not signal the running server ({reason}); run `phux config reload` or \
                 restart it"
            ),
        }
    );
    ExitCode::SUCCESS
}

fn rfc3339(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0).map_or_else(
        || format!("unix {seconds}"),
        |stamp| stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}
