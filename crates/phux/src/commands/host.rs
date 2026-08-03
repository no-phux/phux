//! `phux host` — one visible namespace over the two machine registries
//! (ADR-0066, phux-i0e8.12.2).
//!
//! "Register another machine" is one user intention that the CLI used to
//! fork into two verb trees (`phux remote` and `phux satellite`) by which
//! trust direction the entry encodes. This module absorbs that split into a
//! `--role remote|satellite` axis (default `remote`) while the *storage*
//! stays deliberately split: `--role remote` operates on `[[remote]]` (a
//! server this consumer dials for itself), `--role satellite` on
//! `[[satellites]]` (a peer a hub dials for its users). Everything delegates
//! to the sibling registry modules — [`super::remote`] and
//! [`super::satellite::registry`] — so the trust boundary the config schema
//! defends is never crossed here.
//!
//! `host ls --json` emits one stable document (`schema_version` 1):
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "hosts": [
//!     {
//!       "name": "mini",
//!       "role": "remote",
//!       "endpoint": "quic://mini:8788",
//!       "enabled": null,
//!       "token_file": "/path/to/token",
//!       "cert_fingerprint": "ab..64 hex..",
//!       "session": null
//!     }
//!   ]
//! }
//! ```
//!
//! `enabled` is `null` for remotes (the schema has no enabled bit) and
//! `session` is `null` for satellites (a hub-dialed link has no arrival to
//! attach). `host add --json` wraps one such object under `"host"`;
//! `host rm --json` emits `{"schema_version":1,"removed":{"name":..,"role":..}}`.
//! Failures follow the shared JSON error contract in [`super::json_err`].

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};

use super::JsonOpt;
use super::json_err::{self, CliError, codes};
use super::remote;
use super::satellite::registry as satellite_registry;

/// Which machine registry a `phux host` operation applies to.
///
/// The two roles are stored apart on purpose: a *remote* is a server this
/// machine dials on behalf of itself; a *satellite* is a peer a federation
/// hub dials on behalf of its users. The flag names the trust direction, the
/// verb stays one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub(crate) enum HostRole {
    /// A server this machine attaches to — a `[[remote]]` entry.
    Remote,
    /// A peer this hub dials for its users — a `[[satellites]]` entry.
    Satellite,
}

impl HostRole {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Satellite => "satellite",
        }
    }

    const fn other(self) -> Self {
        match self {
            Self::Remote => Self::Satellite,
            Self::Satellite => Self::Remote,
        }
    }
}

/// `phux host <action>` — CRUD over both machine registries through one
/// namespace.
#[derive(Debug, Subcommand)]
pub(crate) enum HostAction {
    /// Register a machine, or replace an entry with the same name.
    ///
    /// `--role remote` (the default) registers a server `phux attach <name>`
    /// can dial; `--role satellite` registers a peer this hub dials for its
    /// users. Updating replaces the whole entry, so repeat `--token-file` /
    /// `--cert-fingerprint` when re-adding a name or the auth material is
    /// cleared.
    Add {
        /// Local label for the machine.
        name: String,

        /// Endpoint URI: `quic://HOST:PORT`, `wss://HOST:PORT`, or
        /// `ssh://HOST`. `ssh://` rides your existing ssh trust and needs no
        /// pairing; the other two need a token and a certificate pin.
        endpoint: String,

        /// Which registry the entry lands in.
        #[arg(long, value_enum, default_value = "remote")]
        role: HostRole,

        /// Absolute path to a file holding the pairing token minted by
        /// `phux pair` on the other machine.
        #[arg(long, value_name = "PATH")]
        token_file: Option<PathBuf>,

        /// The other machine's TLS certificate SHA-256 fingerprint, as
        /// printed by `phux pair`. Required for `quic://` and `wss://`.
        #[arg(long, value_name = "FP")]
        cert_fingerprint: Option<String>,

        /// Session to attach on arrival (`--role remote` only). Omitted:
        /// the remote server's own last-attach memory decides.
        #[arg(long, value_name = "NAME")]
        session: Option<String>,

        /// Register the entry but leave it disabled (`--role satellite`
        /// only).
        #[arg(long)]
        disabled: bool,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// List registered machines from both registries.
    ///
    /// With no `--role`, remotes and satellites are merged into one table
    /// with a ROLE column; `--role` filters to one registry.
    #[command(name = "ls", visible_alias = "list")]
    List {
        /// Show only this registry.
        #[arg(long, value_enum)]
        role: Option<HostRole>,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Remove a registered machine. Its token file is left in place.
    ///
    /// With no `--role`, the name is resolved across both registries; a name
    /// registered in both is refused until `--role` disambiguates.
    #[command(name = "rm", visible_alias = "remove")]
    Remove {
        /// Registered name.
        name: String,

        /// Which registry to remove from. Omitted: both are searched.
        #[arg(long, value_enum)]
        role: Option<HostRole>,

        #[command(flatten)]
        json: JsonOpt,
    },
}

pub(crate) fn run_host(action: &HostAction) -> ExitCode {
    match action {
        HostAction::Add {
            name,
            endpoint,
            role,
            token_file,
            cert_fingerprint,
            session,
            disabled,
            json,
        } => run_add(&AddArgs {
            name,
            endpoint,
            role: *role,
            token_file: token_file.as_deref(),
            cert_fingerprint: cert_fingerprint.as_deref(),
            session: session.as_deref(),
            disabled: *disabled,
            json: json.json,
        }),
        HostAction::List { role, json } => run_list(*role, json.json),
        HostAction::Remove { name, role, json } => run_remove(name, *role, json.json),
    }
}

/// One `host add` invocation, bundled so the flow reads as data in, entry
/// out instead of an eight-argument call.
struct AddArgs<'a> {
    name: &'a str,
    endpoint: &'a str,
    role: HostRole,
    token_file: Option<&'a std::path::Path>,
    cert_fingerprint: Option<&'a str>,
    session: Option<&'a str>,
    disabled: bool,
    json: bool,
}

/// One merged view of an entry from either registry, shaped for the table
/// and the JSON document.
struct HostRow {
    name: String,
    role: HostRole,
    endpoint: String,
    /// `Some` for satellites; `None` for remotes, whose schema has no
    /// enabled bit.
    enabled: Option<bool>,
    token_file: Option<PathBuf>,
    cert_fingerprint: Option<String>,
    /// `Some` only for remotes; a hub-dialed satellite link has no arrival
    /// to attach.
    session: Option<String>,
}

impl HostRow {
    fn from_remote(entry: remote::RemoteEntry) -> Self {
        Self {
            name: entry.name,
            role: HostRole::Remote,
            endpoint: entry.endpoint,
            enabled: None,
            token_file: entry.token_file,
            cert_fingerprint: entry.cert_fingerprint,
            session: entry.session,
        }
    }

    fn from_satellite(entry: satellite_registry::SatelliteEntry) -> Self {
        Self {
            name: entry.name,
            role: HostRole::Satellite,
            endpoint: entry.endpoint,
            enabled: Some(entry.enabled),
            token_file: entry.token_file,
            cert_fingerprint: entry.cert_fingerprint,
            session: None,
        }
    }
}

/// Refuse a flag paired with the role it does not apply to, naming the
/// remedy. Returns `None` when the pairing is coherent.
///
/// Enforced post-parse because clap cannot make a flag's validity depend on
/// another flag's *value* — and a silent ignore would be worse than either.
fn role_flag_mismatch(role: HostRole, has_session: bool, has_disabled: bool) -> Option<CliError> {
    match role {
        HostRole::Satellite if has_session => Some(CliError::new(
            codes::REGISTRY,
            "--session applies to --role remote only: a satellite link is \
             hub-dialed, so there is no arrival to attach",
            "drop --session, or register the machine as a remote \
             (`phux host add NAME ENDPOINT --session NAME`)",
        )),
        HostRole::Remote if has_disabled => Some(CliError::new(
            codes::REGISTRY,
            "--disabled applies to --role satellite only: a remote entry has \
             no enabled bit",
            "drop --disabled, or register the machine with \
             `phux host add --role satellite NAME ENDPOINT --disabled`",
        )),
        _ => None,
    }
}

/// `phux host add`.
fn run_add(args: &AddArgs<'_>) -> ExitCode {
    if let Some(err) = role_flag_mismatch(args.role, args.session.is_some(), args.disabled) {
        return json_err::emit(args.json, &err, 2);
    }
    let row = match args.role {
        HostRole::Remote => add_remote(args),
        HostRole::Satellite => add_satellite(args),
    };
    let row = match row {
        Ok(row) => row,
        Err(err) => return json_err::emit(args.json, &err, 1),
    };

    if args.json {
        return print_doc(&serde_json::json!({
            "schema_version": 1,
            "host": row_json(&row),
        }));
    }
    let state = match row.enabled {
        Some(false) => " (disabled)",
        _ => "",
    };
    outln!(
        "Registered {} {:?} -> {}{state}",
        row.role.as_str(),
        row.name,
        row.endpoint
    );
    match row.role {
        HostRole::Remote => outln!("Attach with `phux attach {}`.", row.name),
        HostRole::Satellite => {
            outln!("Hub route ready once this host runs `phux service install --hub`.");
        }
    }
    ExitCode::SUCCESS
}

fn add_remote(args: &AddArgs<'_>) -> Result<HostRow, CliError> {
    let new = remote::NewRemote::new(
        args.name,
        args.endpoint,
        args.token_file,
        args.cert_fingerprint,
        args.session,
    )
    .map_err(reject_entry)?;
    remote::add_or_update(&new).map_err(registry_failure)?;
    Ok(HostRow {
        name: new.name,
        role: HostRole::Remote,
        endpoint: new.endpoint,
        enabled: None,
        token_file: new.token_file,
        cert_fingerprint: new.cert_fingerprint,
        session: new.session,
    })
}

fn add_satellite(args: &AddArgs<'_>) -> Result<HostRow, CliError> {
    let new = satellite_registry::NewSatellite::new(
        args.name,
        args.endpoint,
        !args.disabled,
        args.token_file,
        args.cert_fingerprint,
    )
    .map_err(reject_entry)?;
    let entry = satellite_registry::add_or_update(&new).map_err(registry_failure)?;
    Ok(HostRow::from_satellite(entry))
}

/// A rejected field (bad endpoint scheme, missing pin, sigil name): the
/// registry's own message already names the fix.
fn reject_entry(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the flagged field and rerun `phux host add`",
    )
}

/// A registry read/write failure underneath a validated request.
fn registry_failure(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the reported [[remote]] / [[satellites]] entry; \
         `phux config path` names the config file",
    )
}

/// `phux host ls`.
fn run_list(role: Option<HostRole>, json: bool) -> ExitCode {
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
        return print_doc(&serde_json::json!({
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
fn sort_rows(rows: &mut [HostRow]) {
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.role.cmp(&b.role)));
}

/// The teaching empty state: name the command that fills the table, per
/// role when the listing was filtered.
const fn empty_state(role: Option<HostRole>) -> &'static str {
    match role {
        None => {
            "No hosts registered.\n\
             Add a server to attach to with `phux host add NAME ENDPOINT` (or `phux enroll HOST`).\n\
             Add a satellite this hub dials with `phux host add --role satellite NAME ENDPOINT`."
        }
        Some(HostRole::Remote) => {
            "No remote hosts registered.\n\
             Add one with `phux host add NAME ENDPOINT` (or `phux enroll HOST`)."
        }
        Some(HostRole::Satellite) => {
            "No satellite hosts registered.\n\
             Add one with `phux host add --role satellite NAME ENDPOINT`."
        }
    }
}

/// How the link authenticates, classified from the endpoint's transport and
/// the entry's auth material. `ssh://` rides the operator's existing ssh
/// trust; the paired transports report which halves of the token + pin pair
/// are on file.
fn auth_display(endpoint: &str, has_token: bool, has_pin: bool) -> &'static str {
    if endpoint.trim().starts_with("ssh://") {
        return "ssh";
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
fn render_table(rows: &[HostRow]) -> String {
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

fn row_json(row: &HostRow) -> serde_json::Value {
    serde_json::json!({
        "name": row.name,
        "role": row.role.as_str(),
        "endpoint": row.endpoint,
        "enabled": row.enabled,
        // Auth material by reference only: the token-file *path* is
        // machine-readable, the token bytes behind it never appear.
        "token_file": row.token_file.as_ref().map(|p| p.display().to_string()),
        "cert_fingerprint": row.cert_fingerprint,
        "session": row.session,
    })
}

fn print_doc(doc: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(doc) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        // Only reached on a `--json` path, so the failure is the contract
        // line, never prose.
        Err(err) => json_err::emit(
            true,
            &CliError::new(
                codes::JSON_SERIALIZE,
                format!("could not render host JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}

/// Decide which registry `host rm NAME` removes from, given where the name
/// was found. Pure, so the ambiguity and wrong-role remedies are
/// unit-testable without a config file.
///
/// A name in both registries with no `--role` is refused (exit 2) rather
/// than resolved by the default role: `rm` is destructive, and guessing
/// which trust direction the operator meant would delete the wrong entry
/// half the time.
fn resolve_rm_role(
    name: &str,
    requested: Option<HostRole>,
    in_remote: bool,
    in_satellite: bool,
) -> Result<HostRole, (CliError, u8)> {
    let found = |role| match role {
        HostRole::Remote => in_remote,
        HostRole::Satellite => in_satellite,
    };
    match requested {
        Some(role) if found(role) => Ok(role),
        Some(role) if found(role.other()) => {
            let other = role.other().as_str();
            Err((
                CliError::new(
                    codes::REGISTRY,
                    format!("{name:?} is not registered as a {}", role.as_str()),
                    format!(
                        "it is registered as a {other}; run \
                         `phux host rm --role {other} {name}`"
                    ),
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
                     `phux host rm --role remote {name}` or \
                     `phux host rm --role satellite {name}`"
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

/// `phux host rm`.
fn run_remove(name: &str, role: Option<HostRole>, json: bool) -> ExitCode {
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
            (remote::remove_at(entry.index), entry.token_file)
        }
        HostRole::Satellite => {
            let Some(entry) = satellite_entry else {
                return json_err::emit(
                    json,
                    &registry_failure("satellite entry vanished".into()),
                    1,
                );
            };
            (
                satellite_registry::remove_entry(entry.index),
                entry.token_file,
            )
        }
    };
    if let Err(err) = removed {
        return json_err::emit(json, &registry_failure(err), 1);
    }

    if json {
        return print_doc(&serde_json::json!({
            "schema_version": 1,
            "removed": { "name": name, "role": resolved.as_str() },
        }));
    }
    outln!("Removed {} {name:?}.", resolved.as_str());
    if let Some(path) = token_file {
        outln!("Its token file is still at {}.", path.display());
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        HostRole, HostRow, auth_display, empty_state, render_table, resolve_rm_role,
        role_flag_mismatch, sort_rows,
    };
    use crate::commands::json_err::codes;

    fn row(name: &str, role: HostRole) -> HostRow {
        HostRow {
            name: name.to_owned(),
            role,
            endpoint: "ssh://example".to_owned(),
            enabled: (role == HostRole::Satellite).then_some(true),
            token_file: None,
            cert_fingerprint: None,
            session: None,
        }
    }

    /// A flag paired with the role it does not apply to is refused with a
    /// remedy that names the way out; coherent pairings pass.
    #[test]
    fn role_flag_mismatch_names_the_remedy() {
        let err = role_flag_mismatch(HostRole::Satellite, true, false)
            .expect("--session under --role satellite must be refused");
        assert_eq!(err.code, codes::REGISTRY);
        assert!(err.message.contains("--session"), "got {}", err.message);
        assert!(
            err.message.contains("no arrival to attach"),
            "the message explains WHY: {}",
            err.message
        );
        assert!(err.remedy.contains("drop --session"), "got {}", err.remedy);

        let err = role_flag_mismatch(HostRole::Remote, false, true)
            .expect("--disabled under --role remote must be refused");
        assert!(err.message.contains("--disabled"), "got {}", err.message);
        assert!(
            err.remedy.contains("--role satellite"),
            "the remedy names the role that accepts the flag: {}",
            err.remedy
        );

        // The coherent pairings pass.
        assert!(role_flag_mismatch(HostRole::Remote, true, false).is_none());
        assert!(role_flag_mismatch(HostRole::Satellite, false, true).is_none());
        assert!(role_flag_mismatch(HostRole::Remote, false, false).is_none());
    }

    /// `host rm` with no `--role`: unique names resolve, a doubly-registered
    /// name is refused (exit 2) with both disambiguating commands named, and
    /// a miss is exit 1.
    #[test]
    fn rm_resolves_across_registries_and_refuses_ambiguity() {
        assert_eq!(
            resolve_rm_role("mini", None, true, false).map_err(|_| ()),
            Ok(HostRole::Remote)
        );
        assert_eq!(
            resolve_rm_role("edge", None, false, true).map_err(|_| ()),
            Ok(HostRole::Satellite)
        );

        let (err, exit_code) = resolve_rm_role("both", None, true, true)
            .expect_err("a doubly-registered name must be refused");
        assert_eq!(exit_code, 2, "ambiguity is a refusal, not a miss");
        assert!(
            err.remedy.contains("--role remote both")
                && err.remedy.contains("--role satellite both"),
            "the remedy names both disambiguating commands: {}",
            err.remedy
        );

        let (err, exit_code) =
            resolve_rm_role("ghost", None, false, false).expect_err("a miss errors");
        assert_eq!(exit_code, 1);
        assert!(err.remedy.contains("phux host ls"), "got {}", err.remedy);
    }

    /// `host rm --role X NAME` where the name lives in the OTHER registry:
    /// the error names the role that would have matched instead of reading
    /// as a plain miss.
    #[test]
    fn rm_wrong_role_remedy_names_the_matching_role() {
        let (err, exit_code) = resolve_rm_role("edge", Some(HostRole::Remote), false, true)
            .expect_err("wrong role is a miss with a teaching remedy");
        assert_eq!(exit_code, 1);
        assert!(
            err.remedy.contains("--role satellite edge"),
            "got {}",
            err.remedy
        );

        let (err, _) = resolve_rm_role("mini", Some(HostRole::Satellite), true, false)
            .expect_err("symmetric case");
        assert!(
            err.remedy.contains("--role remote mini"),
            "got {}",
            err.remedy
        );

        // The requested role matching wins even when both registries hold
        // the name — an explicit `--role` is never ambiguous.
        assert_eq!(
            resolve_rm_role("both", Some(HostRole::Satellite), true, true).map_err(|_| ()),
            Ok(HostRole::Satellite)
        );
    }

    /// Merged ordering is by name, remotes before satellites on a shared
    /// name — independent of config-file entry order.
    #[test]
    fn merged_rows_sort_by_name_then_remote_first() {
        let mut rows = vec![
            row("zeta", HostRole::Satellite),
            row("alpha", HostRole::Satellite),
            row("mini", HostRole::Remote),
            row("alpha", HostRole::Remote),
        ];
        sort_rows(&mut rows);
        let order: Vec<_> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.role))
            .collect();
        assert_eq!(
            order,
            vec![
                ("alpha", HostRole::Remote),
                ("alpha", HostRole::Satellite),
                ("mini", HostRole::Remote),
                ("zeta", HostRole::Satellite),
            ]
        );
    }

    /// The AUTH column classifies the transport: ssh rides ssh trust, the
    /// paired transports report which halves of token + pin are on file.
    #[test]
    fn auth_display_classifies_transports() {
        assert_eq!(auth_display("ssh://mini", false, false), "ssh");
        // ssh needs no pairing even if material is (pointlessly) on file.
        assert_eq!(auth_display("  ssh://mini  ", true, true), "ssh");
        assert_eq!(auth_display("quic://mini:8788", true, true), "token+pin");
        assert_eq!(auth_display("wss://mini:8787", false, true), "pin");
        assert_eq!(auth_display("quic://mini:8788", true, false), "token");
        assert_eq!(auth_display("wss://mini:8787", false, false), "none");
    }

    /// The table carries the five pinned columns; STATE is `-` for remotes
    /// and the enabled bit for satellites.
    #[test]
    fn table_renders_role_state_and_auth_columns() {
        let rows = vec![
            HostRow {
                name: "mini".to_owned(),
                role: HostRole::Remote,
                endpoint: "quic://mini:8788".to_owned(),
                enabled: None,
                token_file: Some(PathBuf::from("/tokens/mini")),
                cert_fingerprint: Some("ab".repeat(32)),
                session: Some("work".to_owned()),
            },
            HostRow {
                name: "edge".to_owned(),
                role: HostRole::Satellite,
                endpoint: "ssh://edge".to_owned(),
                enabled: Some(false),
                token_file: None,
                cert_fingerprint: None,
                session: None,
            },
        ];
        let table = render_table(&rows);
        let mut lines = table.lines();
        let header = lines.next().expect("header line");
        for column in ["NAME", "ROLE", "ENDPOINT", "STATE", "AUTH"] {
            assert!(header.contains(column), "header lost {column}: {header}");
        }
        let mini = lines.next().expect("first row");
        assert!(mini.contains("remote") && mini.contains("token+pin"));
        assert!(mini.contains(" - "), "remote STATE is the `-` placeholder");
        let edge = lines.next().expect("second row");
        assert!(edge.contains("satellite") && edge.contains("disabled") && edge.contains("ssh"));
    }

    /// The empty states teach the command that fills the table — both roles
    /// on the merged view, the matching one under a filter.
    #[test]
    fn empty_states_name_the_add_commands() {
        let merged = empty_state(None);
        assert!(merged.contains("phux host add NAME ENDPOINT"));
        assert!(merged.contains("--role satellite"));
        assert!(merged.contains("phux enroll HOST"));

        let remotes = empty_state(Some(HostRole::Remote));
        assert!(remotes.contains("phux host add NAME ENDPOINT"));
        assert!(!remotes.contains("--role satellite"));

        let satellites = empty_state(Some(HostRole::Satellite));
        assert!(satellites.contains("--role satellite"));
    }
}
