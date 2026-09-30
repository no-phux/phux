//! `phux host` — one namespace over the two machine registries (ADR-0066,
//! ADR-0122). `--role remote` (default) operates on `[[remote]]`, servers this
//! machine dials; `--role satellite` on `[[satellites]]`, peers a hub dials for
//! its users. Storage stays split along that trust direction; everything
//! delegates to [`super::remote`] and [`super::satellite`].
//!
//! `phux host add [USER@]HOST` sets a machine up over ssh end to end
//! ([`super::enroll::enroll_over_ssh`]); `NAME ENDPOINT` or an endpoint URI
//! registers credentials minted elsewhere.
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
//!       "session": null,
//!       "ssh": "me@mini",
//!       "direct": null,
//!       "client_cert": "/path/to/mini.client.0123456789abcdef.pem",
//!       "client_key": "/path/to/mini.client.0123456789abcdef.key"
//!     }
//!   ]
//! }
//! ```
//!
//! `enabled` is `null` for remotes; `session`, `ssh`, `direct`, `client_cert`,
//! and `client_key` are `null` for satellites. The client identity is named
//! by path only; the key bytes never appear. `host add --json` wraps one such object under `"host"`;
//! `host rm --json` emits `{"schema_version":1,"removed":{"name":..,"role":..}}`.
//! Failures follow the shared JSON error contract in [`super::json_err`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use toml_edit::{Item, value};
use usage::{Args, Subcommands, ValueEnum};

use super::JsonOpt;
use super::enroll::{
    self, ClientIdentityFiles, EnrollEvent, EnrollFailure, EnrollRequest, ServicePolicy,
    WorkloadEnrollment,
};
use super::json_err::{self, CliError, codes};
use super::remote::{self, Endpoint, RemoteEntry};
use super::remote_target::{RemoteTarget, endpoint_host};
use super::satellite as satellite_registry;
use super::service;

/// Which machine registry a `phux host` operation applies to: the flag names
/// the trust direction, the verb stays one.
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

    /// The `--role` flag to paste into a remedy, empty for the default.
    const fn flag(self) -> &'static str {
        match self {
            Self::Remote => "",
            Self::Satellite => " --role satellite",
        }
    }
}

/// The flags `phux host add` takes, shared with its hidden `enroll`
/// spelling.
#[derive(Debug, Args)]
pub(crate) struct AddOpts {
    /// Which registry the machine lands in: a server you attach to
    /// (`remote`, the default) or a peer this hub dials for its users.
    #[usage(long, value_enum, default = "remote")]
    pub(crate) role: HostRole,

    /// Local label to register. Defaults to HOST without any `user@`, or
    /// to the host of an endpoint URI.
    #[usage(long, value_name = "NAME")]
    pub(crate) name: Option<String>,

    /// Register this address instead of the routes detected on the host:
    /// `HOST:PORT` (dialed over QUIC) or a full `quic://`/`wss://` URI.
    #[usage(long = "endpoint", value_name = "HOST:PORT")]
    pub(crate) endpoint_override: Option<String>,

    /// QUIC port to configure on the host and dial.
    #[usage(long, value_name = "PORT", default = "8788", default_value_t = enroll::default_quic_port())]
    pub(crate) quic_port: u16,

    /// Start the host's server but skip installing its service unit. The
    /// server will not come back on its own after a reboot.
    #[usage(long)]
    pub(crate) no_service: bool,

    /// Register an `ssh://HOST` entry without contacting the host at all.
    #[usage(long, conflicts("--endpoint", "--no-service"))]
    pub(crate) ssh_only: bool,

    /// The `phux` to run on the host, for when a non-interactive ssh
    /// shell's `PATH` does not find it (a Homebrew or Nix install).
    #[usage(long, value_name = "PATH", default = "phux")]
    pub(crate) remote_phux: String,

    /// Manual form only: absolute path to a file holding the pairing token
    /// minted by `phux pair` on the other machine.
    #[usage(long, value_name = "PATH")]
    pub(crate) token_file: Option<PathBuf>,

    /// Manual form only: the other machine's TLS certificate SHA-256
    /// fingerprint, as printed by `phux pair`. Required for `quic://` and
    /// `wss://`.
    #[usage(long, value_name = "FP")]
    pub(crate) cert_fingerprint: Option<String>,

    /// Session to attach on arrival (`--role remote` only). Omitted: the
    /// remote server's own last-attach memory decides.
    #[usage(long, value_name = "NAME")]
    pub(crate) session: Option<String>,

    /// Register the entry but leave it disabled (`--role satellite` only).
    #[usage(long)]
    pub(crate) disabled: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// `phux host <action>` — CRUD over both machine registries through one
/// namespace.
#[derive(Debug, Subcommands)]
pub(crate) enum HostAction {
    /// Add a machine so `phux attach NAME` reaches it.
    ///
    /// `phux host add me@mini` is all a new machine needs. Over the ssh
    /// trust you already have it confirms phux is installed there, starts
    /// the server and keeps it running across reboots, pairs, tries the
    /// direct routes, and registers the first one that answers. Nothing to
    /// type by hand: no port, no token, no fingerprint. Run it again on a
    /// registered machine to check it and repair it if it stopped
    /// answering.
    ///
    /// The manual form registers an endpoint you already hold credentials
    /// for: `phux host add NAME quic://HOST:PORT --token-file PATH
    /// --cert-fingerprint FP` (`wss://` likewise; `ssh://HOST` needs no
    /// credentials). It replaces the whole entry, so repeat the credential
    /// flags when re-adding a name. `--role satellite` registers a peer
    /// this hub dials for its users instead of a server you attach to.
    Add {
        /// `[USER@]HOST[:PORT]` to set up over ssh, or an endpoint URI
        /// (`quic://`, `wss://`, `ssh://`) to register as is.
        target: String,

        /// Manual form: the endpoint URI to register, with TARGET as the
        /// entry's name.
        endpoint: Option<String>,

        #[usage(flatten)]
        opts: AddOpts,
    },

    /// List registered machines from both registries.
    ///
    /// With no `--role`, remotes and satellites are merged into one table
    /// with a ROLE column; `--role` filters to one registry.
    #[usage(name = "ls", alias = "list")]
    List {
        /// Show only this registry.
        #[usage(long, value_enum)]
        role: Option<HostRole>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Remove a registered machine. Its token file is left in place.
    ///
    /// With no `--role`, the name is resolved across both registries; a name
    /// registered in both is refused until `--role` disambiguates.
    #[usage(name = "rm", alias = "remove")]
    Remove {
        /// Registered name.
        name: String,

        /// Which registry to remove from. Omitted: both are searched.
        #[usage(long, value_enum)]
        role: Option<HostRole>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Show a registered machine, including its route and credential references.
    Show {
        /// Registered name.
        name: String,
        /// Disambiguate a name present in both registries.
        #[usage(long, value_enum)]
        role: Option<HostRole>,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Rename a registered machine without changing its route or credentials.
    Rename {
        /// Current registered name.
        name: String,
        /// New local label (does not rename the machine over the network).
        new_name: String,
        #[usage(long, value_enum)]
        role: Option<HostRole>,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Attach to a registered remote host (same as `phux attach NAME`).
    Attach {
        /// Registered remote name.
        name: String,
    },

    /// Enable a satellite for the local hub on its next start.
    Enable {
        /// Registered satellite name.
        name: String,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Disable a satellite on the hub's next start without forgetting it.
    Disable {
        /// Registered satellite name.
        name: String,
        #[usage(flatten)]
        json: JsonOpt,
    },
}

pub(crate) fn run_host(action: &HostAction) -> ExitCode {
    match action {
        HostAction::Add {
            target,
            endpoint,
            opts,
        } => run_add(target, endpoint.as_deref(), opts),
        HostAction::List { role, json } => run_list(*role, json.json),
        HostAction::Remove { name, role, json } => run_remove(name, *role, json.json),
        HostAction::Show { name, role, json } => run_show(name, *role, json.json),
        HostAction::Rename {
            name,
            new_name,
            role,
            json,
        } => run_rename(name, new_name, *role, json.json),
        HostAction::Attach { name } => run_attach(name),
        HostAction::Enable { name, json } => run_enabled(name, true, json.json),
        HostAction::Disable { name, json } => run_enabled(name, false, json.json),
    }
}

/// One merged view of an entry from either registry, shaped for the table
/// and the JSON document.
#[derive(Debug)]
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
    /// Remote only: the ssh destination the entry was enrolled through.
    ssh: Option<String>,
    /// Remote only: a paired direct route kept beside an `ssh://` endpoint.
    direct: Option<String>,
    /// Remote only: the enrolled workload client certificate chain.
    client_cert: Option<PathBuf>,
    /// Remote only: its private key's path (never its bytes).
    client_key: Option<PathBuf>,
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
            ssh: entry.ssh,
            direct: entry.direct,
            client_cert: entry.client_cert,
            client_key: entry.client_key,
        }
    }

    fn from_new_remote(new: remote::NewRemote) -> Self {
        Self {
            name: new.name,
            role: HostRole::Remote,
            endpoint: new.endpoint,
            enabled: None,
            token_file: new.token_file,
            cert_fingerprint: new.cert_fingerprint,
            session: new.session,
            ssh: new.ssh,
            direct: new.direct,
            client_cert: new.client_cert,
            client_key: new.client_key,
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
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        }
    }
}

/// Refuse a flag paired with the role it does not apply to, naming the
/// remedy. Post-parse: the parser cannot condition a flag on another's value.
fn role_flag_mismatch(role: HostRole, has_session: bool, has_disabled: bool) -> Option<CliError> {
    match role {
        HostRole::Satellite if has_session => Some(CliError::new(
            codes::REGISTRY,
            "--session applies to --role remote only: a satellite link is \
             hub-dialed, so there is no arrival to attach",
            "drop --session, or register the machine as a remote \
             (`phux host add HOST --session NAME`)",
        )),
        HostRole::Remote if has_disabled => Some(CliError::new(
            codes::REGISTRY,
            "--disabled applies to --role satellite only: a remote entry has \
             no enabled bit",
            "drop --disabled, or register the machine with \
             `phux host add --role satellite HOST --disabled`",
        )),
        _ => None,
    }
}

/// Which of `host add`'s two forms an invocation is, decided from the
/// arguments alone so the rule is one place and testable.
#[derive(Debug, PartialEq, Eq)]
enum AddMode {
    /// Register `endpoint` under `name` from what was typed.
    Manual { name: String, endpoint: String },
    /// Set `target` up over ssh.
    Ssh { target: String },
}

impl AddMode {
    /// `NAME ENDPOINT` and a bare endpoint URI are the manual form; anything
    /// else is an ssh destination. A flag from the other form is refused.
    fn classify(target: &str, endpoint: Option<&str>, opts: &AddOpts) -> Result<Self, CliError> {
        let manual = if let Some(endpoint) = endpoint {
            if opts.name.is_some() {
                return Err(CliError::new(
                    codes::REGISTRY,
                    "--name does not combine with a NAME ENDPOINT pair: the first argument is the name",
                    "drop --name, or pass only the endpoint URI with --name NAME",
                ));
            }
            Some((target.to_owned(), endpoint.to_owned()))
        } else if target.contains("://") {
            let name = match &opts.name {
                Some(name) => name.clone(),
                None => endpoint_host(target).ok_or_else(|| {
                    CliError::new(
                        codes::REGISTRY,
                        format!("could not derive a name from {target:?}"),
                        "pass --name NAME, or `phux host add NAME ENDPOINT`",
                    )
                })?,
            };
            Some((name, target.to_owned()))
        } else {
            None
        };

        let Some((name, endpoint)) = manual else {
            if opts.token_file.is_some() || opts.cert_fingerprint.is_some() {
                return Err(CliError::new(
                    codes::REGISTRY,
                    "--token-file and --cert-fingerprint belong to the manual form, which registers an endpoint you already hold credentials for",
                    format!(
                        "pass the endpoint too (`phux host add {target} quic://HOST:PORT --token-file PATH --cert-fingerprint FP`), \
                         or drop them and let `phux host add {target}` pair over ssh"
                    ),
                ));
            }
            return Ok(Self::Ssh {
                target: target.to_owned(),
            });
        };
        if let Some(flag) = ssh_form_flag_given(opts) {
            return Err(CliError::new(
                codes::REGISTRY,
                format!(
                    "{flag} belongs to the ssh form (`phux host add [USER@]HOST`), which sets the machine up itself"
                ),
                format!(
                    "drop {flag}, or drop the endpoint URI and let `phux host add HOST` pair over ssh"
                ),
            ));
        }
        Ok(Self::Manual { name, endpoint })
    }
}

/// The first ssh-form flag present on a manual invocation, by its spelling.
fn ssh_form_flag_given(opts: &AddOpts) -> Option<&'static str> {
    if opts.ssh_only {
        Some("--ssh-only")
    } else if opts.endpoint_override.is_some() {
        Some("--endpoint")
    } else if opts.no_service {
        Some("--no-service")
    } else if opts.remote_phux != "phux" {
        Some("--remote-phux")
    } else if opts.quic_port != enroll::default_quic_port() {
        Some("--quic-port")
    } else {
        None
    }
}

/// `phux host add`.
fn run_add(target: &str, endpoint: Option<&str>, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    if let Some(err) = role_flag_mismatch(opts.role, opts.session.is_some(), opts.disabled) {
        return json_err::emit(json, &err, 2);
    }
    match AddMode::classify(target, endpoint, opts) {
        Err(err) => json_err::emit(json, &err, 2),
        Ok(AddMode::Manual { name, endpoint }) => run_add_manual(&name, &endpoint, opts),
        Ok(AddMode::Ssh { target }) => run_add_over_ssh(&target, opts),
    }
}

/// The manual form: register exactly what was typed.
fn run_add_manual(name: &str, endpoint: &str, opts: &AddOpts) -> ExitCode {
    let row = match opts.role {
        HostRole::Remote => remote::NewRemote::new(
            name,
            endpoint,
            opts.token_file.as_deref(),
            opts.cert_fingerprint.as_deref(),
            opts.session.as_deref(),
        )
        .map_err(reject_entry)
        .and_then(|new| {
            remote::add_or_update(&new).map_err(registry_failure)?;
            Ok(HostRow::from_new_remote(new))
        }),
        HostRole::Satellite => satellite_registry::NewSatellite::new(
            name,
            endpoint,
            !opts.disabled,
            opts.token_file.as_deref(),
            opts.cert_fingerprint.as_deref(),
        )
        .map_err(reject_entry)
        .and_then(|new| {
            satellite_registry::add_or_update(&new)
                .map(HostRow::from_satellite)
                .map_err(registry_failure)
        }),
    };
    let row = match row {
        Ok(row) => row,
        Err(err) => return json_err::emit(opts.json.json, &err, 1),
    };
    let how = match (row.enabled, row.endpoint.starts_with("ssh://")) {
        (Some(false), _) => "disabled",
        (_, true) => "rides your ssh trust",
        (_, false) => "credentials as given",
    };
    report_registered(&row, how, &[], opts.json.json, None)
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

/// Map a failure from the shared ssh middle onto the CLI error contract,
/// with a remedy the operator can paste per failure class.
fn add_failure_error(ssh_host: &str, role: HostRole, failure: &EnrollFailure) -> CliError {
    // A remedy the operator can paste verbatim: it must carry the role they
    // asked for, or following it would register into the wrong registry.
    let role_flag = role.flag();
    match failure {
        EnrollFailure::SshUnreachable(detail) => CliError::new(
            codes::REGISTRY,
            format!("cannot reach {ssh_host} over ssh: {detail}"),
            format!(
                "check that `ssh {ssh_host}` works from this machine, then rerun \
                 `phux host add {ssh_host}{role_flag}`; or register it without contacting it: \
                 `phux host add {ssh_host}{role_flag} --ssh-only`"
            ),
        ),
        EnrollFailure::MissingPhux(detail) => CliError::new(
            codes::REGISTRY,
            format!("{ssh_host} has no phux on the PATH its ssh shell sees: {detail}"),
            format!(
                "install it there: `ssh {ssh_host} 'curl -fsSL https://phux.sh/install | sh'`; \
                 or, if it is installed where a non-interactive shell does not look, name it: \
                 `phux host add {ssh_host}{role_flag} --remote-phux ~/.local/bin/phux`"
            ),
        ),
        EnrollFailure::Pair(detail) => CliError::new(
            codes::REGISTRY,
            detail.clone(),
            format!(
                "look at the host (`ssh {ssh_host} phux doctor`) and rerun \
                 `phux host add {ssh_host}{role_flag}`, or register an ssh-only entry: \
                 `phux host add {ssh_host}{role_flag} --ssh-only`"
            ),
        ),
    }
}

/// A field the target registry rejected after enrollment (bad endpoint
/// scheme, missing pin, sigil name): the registry's message names the fix.
fn reject_enrollment(message: String) -> CliError {
    CliError::new(
        codes::REGISTRY,
        message,
        "fix the flagged field and rerun `phux host add`",
    )
}

/// The ssh form of `phux host add`: set the machine up end to end.
fn run_add_over_ssh(raw_target: &str, opts: &AddOpts) -> ExitCode {
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

    let narrate = |event: &EnrollEvent| {
        if !json {
            eprintln!("{name}: {}", event.describe());
        }
    };

    // Already registered, answering, and holding a workload client
    // certificate: say so and stop, rather than re-pairing a machine that
    // needs nothing. One without a certificate goes on to enroll one.
    if opts.role == HostRole::Remote
        && let Some(entry) = remote::find(&name)
        && entry.client_identity_paths().is_some()
    {
        match saved_route_answers(&entry) {
            Some(Ok(())) => {
                let row = HostRow::from_remote(entry);
                return report_registered(
                    &row,
                    "already registered and answering; nothing to do",
                    &[],
                    json,
                    None,
                );
            }
            Some(Err(reason)) => narrate(&EnrollEvent::DirectUnreachable {
                endpoint: entry.endpoint.clone(),
                reason: format!("{reason}; setting the machine up again over ssh"),
            }),
            None => {}
        }
    }

    // Prefer credentials already on file for this name when re-enrolling so
    // a stopped server does not mint another live bearer into remote-tokens.
    let previous = previous_enrollment(&name, opts.role);
    let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
    let req = EnrollRequest {
        ssh_host: &ssh_host,
        remote_phux: &opts.remote_phux,
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
        &outcome,
        opts.session.as_deref(),
        Some(&ssh_host),
        previous.identity.as_ref(),
    );
    match registered {
        Ok(row) => report_ssh_add(&row, &outcome, &ssh_host, quic_port, opts),
        Err(err) => json_err::emit(json, &err, 1),
    }
}

/// `--ssh-only` skips the host entirely: no pairing, no service, no
/// certificate, just a registry entry riding existing ssh trust.
fn run_add_ssh_only(name: &str, ssh_host: &str, opts: &AddOpts) -> ExitCode {
    let json = opts.json.json;
    let endpoint = format!("ssh://{ssh_host}");
    let registered = finish_enroll(
        opts.role,
        name,
        &Route::bare(&endpoint),
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
        ),
        Err(err) => json_err::emit(json, &err, 1),
    }
}

/// The summary after the ssh form registered a machine: how it connects,
/// what was tried when nothing answered, and what to type next.
fn report_ssh_add(
    row: &HostRow,
    outcome: &enroll::EnrollOutcome,
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
    )
}

/// Whether a registered entry's saved direct route answers right now.
/// `None` when there is nothing to dial without ssh (an `ssh://` route, a
/// `wss://` one the QUIC probe cannot speak, or missing credentials).
fn saved_route_answers(entry: &RemoteEntry) -> Option<Result<(), String>> {
    let Ok(Endpoint::Quic(target)) = Endpoint::parse(&entry.endpoint) else {
        return None;
    };
    let token = remote::read_token(entry).ok().flatten()?;
    let identity = entry.client_identity().ok()?;
    Some(enroll::probe(
        &target,
        &token,
        entry.cert_fingerprint.as_deref(),
        identity,
    ))
}

/// Token, pin, and client identity already held for `name`, when
/// re-enrolling a remote.
#[derive(Default)]
pub(crate) struct PreviousEnrollment {
    pub(crate) token: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) identity: Option<ClientIdentityFiles>,
}

impl PreviousEnrollment {
    /// What `entry` holds.
    pub(crate) fn of(entry: &RemoteEntry) -> Self {
        Self {
            token: remote::read_token(entry).ok().flatten(),
            fingerprint: entry.cert_fingerprint.clone(),
            identity: entry
                .client_identity_paths()
                .map(|(cert, key)| ClientIdentityFiles::existing(cert, key)),
        }
    }
}

fn previous_enrollment(name: &str, role: HostRole) -> PreviousEnrollment {
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
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Result<(RemoteEntry, enroll::EnrollOutcome), CliError> {
    let outcome = enroll::enroll_over_ssh(req, &mut |event| narrate(&event))
        .map_err(|failure| add_failure_error(req.ssh_host, HostRole::Remote, &failure))?;
    register_enrollment(
        HostRole::Remote,
        name,
        &outcome,
        session,
        Some(req.ssh_host),
        req.previous_identity,
    )?;
    let entry = remote::find(name)
        .ok_or_else(|| registry_failure(format!("{name:?} was written but cannot be read back")))?;
    Ok((entry, outcome))
}

/// `--role satellite` has to leave this machine able to dial the new peer.
/// `--role remote` is the opposite trust direction and must not touch the
/// local unit.
fn local_hub_for(role: HostRole) -> Option<service::LocalHub> {
    (role == HostRole::Satellite).then(service::ensure_local_hub)
}

/// Register what the ssh middle produced, then settle the workload client
/// identity files: a fresh identity the entry did not record is removed, and
/// once the entry names a new identity the previous one's files go, when an
/// enrollment wrote them (never files the operator named themselves). The
/// registry write is the one switch between the two pairs, so a reader
/// always sees a whole old pair or a whole new one.
fn register_enrollment(
    role: HostRole,
    name: &str,
    outcome: &enroll::EnrollOutcome,
    session: Option<&str>,
    ssh: Option<&str>,
    previous: Option<&ClientIdentityFiles>,
) -> Result<HostRow, CliError> {
    let route = Route {
        endpoint: &outcome.endpoint,
        pairing: Some(&outcome.report),
        direct: outcome.direct.as_deref(),
        identity: outcome.identity.as_ref(),
    };
    let registered = finish_enroll(role, name, &route, session, ssh);
    let recorded = registered
        .as_ref()
        .ok()
        .and_then(|row| row.client_cert.as_deref());
    if let Some(identity) = &outcome.identity
        && recorded != Some(identity.certificate.as_path())
    {
        identity.discard_if_fresh();
    }
    let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
    if let (Ok(_), Some(previous)) = (&registered, previous)
        && recorded != Some(previous.certificate.as_path())
        && previous.enrolled_under(&remotes_dir, name)
    {
        phux_server::workload::remove_identity_files(&previous.private_key, &previous.certificate);
    }
    registered
}

/// Where an enrolled entry points and what it authenticates with.
struct Route<'a> {
    /// The endpoint to register.
    endpoint: &'a str,
    /// The pairing document (`None` for `--ssh-only`).
    pairing: Option<&'a enroll::PairReport>,
    /// A direct route kept beside an `ssh://` endpoint.
    direct: Option<&'a str>,
    /// The workload client identity to record (remotes only).
    identity: Option<&'a ClientIdentityFiles>,
}

impl<'a> Route<'a> {
    /// An endpoint with no credentials at all (`--ssh-only`).
    const fn bare(endpoint: &'a str) -> Self {
        Self {
            endpoint,
            pairing: None,
            direct: None,
            identity: None,
        }
    }
}

/// The role-specific tail of the ssh form: validate the entry, write the
/// pairing token under `remotes/` or `satellites/`, and register it.
/// Validation runs before the token hits disk so a rejected entry never
/// leaves an orphaned bearer token.
fn finish_enroll(
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
fn finish_enroll_in(
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
            .and_then(|new| new.with_ssh(ssh).with_direct(direct))
            .and_then(|new| new.with_client_identity(identity))
            .map_err(reject_enrollment)?;
            write_pairing_token(token_file.as_deref(), pairing)?;
            remote::add_or_update(&new).map_err(registry_failure)?;
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
fn keeps_credentials(role: HostRole, endpoint: &str, direct: Option<&str>) -> bool {
    !endpoint.starts_with("ssh://") || (role == HostRole::Remote && direct.is_some())
}

/// Write the pairing token owner-only, when the entry has one to write.
fn write_pairing_token(
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
fn report_registered(
    row: &HostRow,
    how: &str,
    notes: &[String],
    json: bool,
    hub: Option<&service::LocalHub>,
) -> ExitCode {
    if json {
        let mut doc = serde_json::json!({
            "schema_version": 1,
            "host": row_json(row),
        });
        if let Some(hub) = hub {
            doc["hub_service"] = serde_json::Value::String(hub.as_json_str().to_owned());
        }
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
        HostRole::Satellite => report_local_hub(hub),
    }
    ExitCode::SUCCESS
}

fn report_local_hub(hub: Option<&service::LocalHub>) {
    match hub {
        Some(service::LocalHub::Already) => {
            outln!("  local hub service already runs with --hub");
        }
        Some(service::LocalHub::Patched) => {
            outln!("  local hub service: --hub added; existing listeners kept");
        }
        Some(service::LocalHub::Installed) => {
            outln!("  local hub service installed with --hub");
        }
        Some(service::LocalHub::Skipped(reason)) => {
            eprintln!("phux host add: warning: could not enable local --hub: {reason}");
            outln!("  Hub route ready once this host runs `phux service install --hub`.");
        }
        None => {
            outln!("  Hub route ready once this host runs `phux service install --hub`.");
        }
    }
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
fn sort_rows(rows: &mut [HostRow]) {
    rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.role.cmp(&b.role)));
}

/// The teaching empty state: name the command that fills the table, per
/// role when the listing was filtered.
const fn empty_state(role: Option<HostRole>) -> &'static str {
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
        "ssh": row.ssh,
        "direct": row.direct,
        // Paths only, like the token file: the key bytes never appear.
        "client_cert": row.client_cert.as_ref().map(|p| p.display().to_string()),
        "client_key": row.client_key.as_ref().map(|p| p.display().to_string()),
    })
}

/// Decide which registry `host rm NAME` removes from. A name in both with no
/// `--role` is refused (exit 2): `rm` is destructive, so never guess.
fn resolve_rm_role(
    name: &str,
    requested: Option<HostRole>,
    in_remote: bool,
    in_satellite: bool,
) -> Result<HostRole, (CliError, u8)> {
    resolve_host_role(name, requested, in_remote, in_satellite, "rm")
}

fn resolve_host_role(
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

fn find_host(name: &str, role: Option<HostRole>, verb: &str) -> Result<HostRow, (CliError, u8)> {
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

fn run_show(name: &str, role: Option<HostRole>, json: bool) -> ExitCode {
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
    if let Some(session) = row.session {
        outln!("  Session: {session}");
    }
    if let Some(ssh) = row.ssh {
        outln!("  SSH: {ssh}");
    }
    if let Some(direct) = row.direct {
        outln!("  Direct fallback: {direct}");
    }
    if let Some(cert) = row.client_cert {
        outln!("  Client certificate: {}", cert.display());
    }
    ExitCode::SUCCESS
}

fn run_attach(name: &str) -> ExitCode {
    let entries = match remote::load_registry() {
        Ok(entries) => entries,
        Err(err) => return json_err::emit(false, &registry_failure(err), 1),
    };
    if let Some(entry) = entries.into_iter().find(|entry| entry.name == name) {
        return super::remote_target::run_registered(name, entry, None);
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
fn edit_host_field(
    row: &HostRow,
    field: &str,
    replacement: toml_edit::Value,
) -> Result<(), String> {
    let path = phux_config::loader::config_path();
    edit_host_field_at(&path, row, field, replacement)
}

fn edit_host_field_at(
    path: &Path,
    row: &HostRow,
    field: &str,
    replacement: toml_edit::Value,
) -> Result<(), String> {
    let mut doc = super::toml_registry::edit_document(path)?;
    let key = match row.role {
        HostRole::Remote => "remote",
        HostRole::Satellite => "satellites",
    };
    let tables = super::toml_registry::tables_mut(&mut doc, key)?;
    let matches: Vec<_> = tables
        .iter()
        .enumerate()
        .filter_map(|(index, table)| {
            (table.get("name").and_then(Item::as_str) == Some(row.name.as_str())
                && table.get("endpoint").and_then(Item::as_str) == Some(row.endpoint.as_str()))
            .then_some(index)
        })
        .collect();
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

fn run_rename(name: &str, new_name: &str, role: Option<HostRole>, json: bool) -> ExitCode {
    let row = match find_host(name, role, "rename") {
        Ok(row) => row,
        Err((err, code)) => return json_err::emit(json, &err, code),
    };
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
    if json {
        return crate::output::json(
            &serde_json::json!({"schema_version": 1, "renamed": {"from": name, "to": new_name, "role": row.role.as_str()}, "requires_restart": row.role == HostRole::Satellite}),
        );
    }
    outln!("Renamed {} {name:?} to {new_name:?}.", row.role.as_str());
    if row.role == HostRole::Satellite {
        outln!("Restart the hub for this to take effect.");
    }
    ExitCode::SUCCESS
}

fn run_enabled(name: &str, enabled: bool, json: bool) -> ExitCode {
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
    if json {
        return crate::output::json(
            &serde_json::json!({"schema_version": 1, "host": row_json(&row), "requires_restart": true}),
        );
    }
    outln!("Satellite {name:?} {state} in config. Restart the hub for this to take effect.");
    ExitCode::SUCCESS
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

    if json {
        return crate::output::json(&serde_json::json!({
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
    use std::path::{Path, PathBuf};

    use super::{
        AddMode, AddOpts, HostRole, HostRow, Route, add_failure_error, auth_display,
        edit_host_field_at, empty_state, finish_enroll_in, keeps_credentials, render_table,
        resolve_host_role, resolve_rm_role, role_flag_mismatch, sort_rows,
    };
    use crate::commands::JsonOpt;
    use crate::commands::enroll::EnrollFailure;
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
            ssh: None,
            direct: None,
            client_cert: None,
            client_key: None,
        }
    }

    #[test]
    fn host_edits_preserve_other_fields_and_refuse_inherited_or_colliding_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        let original = "# keep this\n[[remote]]\nname = 'mini'\nendpoint = 'quic://example:8788'\ntoken-file = '/some/token'\n[[remote]]\nname = 'other'\nendpoint = 'ssh://other'\n[[satellites]]\nname = 'edge'\nendpoint = 'ssh://example'\nenabled = true\n";
        std::fs::write(&path, original).expect("seed");
        let mut mini = row("mini", HostRole::Remote);
        mini.endpoint = "quic://example:8788".to_owned();
        assert!(edit_host_field_at(&path, &mini, "name", "other".into()).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("unchanged"), original);
        edit_host_field_at(&path, &mini, "name", "desk".into()).expect("rename");
        edit_host_field_at(
            &path,
            &row("edge", HostRole::Satellite),
            "enabled",
            false.into(),
        )
        .expect("disable");
        let changed = std::fs::read_to_string(&path).expect("read");
        assert!(changed.starts_with("# keep this"));
        assert!(changed.contains("token-file = '/some/token'"));
        assert!(
            changed.contains("ssh = \"mini\""),
            "old repair destination survives rename: {changed}"
        );
        assert!(changed.contains("enabled = false"));
        assert!(
            edit_host_field_at(
                &path,
                &row("missing", HostRole::Remote),
                "name",
                "new".into()
            )
            .is_err()
        );
    }

    #[test]
    fn non_destructive_host_lookup_still_refuses_ambiguous_names() {
        let (err, code) =
            resolve_host_role("mini", None, true, true, "rename").expect_err("ambiguous");
        assert_eq!(code, 2);
        assert!(err.remedy.contains("host rename --role remote mini"));
    }

    fn opts() -> AddOpts {
        AddOpts {
            role: HostRole::Remote,
            name: None,
            endpoint_override: None,
            quic_port: 8788,
            no_service: false,
            ssh_only: false,
            remote_phux: "phux".to_owned(),
            token_file: None,
            cert_fingerprint: None,
            session: None,
            disabled: false,
            json: JsonOpt { json: false },
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

    /// The two forms are told apart from the arguments alone: a `NAME
    /// ENDPOINT` pair or a bare URI is manual, anything else is an ssh
    /// destination.
    #[test]
    fn add_mode_is_decided_by_the_arguments() {
        assert_eq!(
            AddMode::classify("mini", Some("quic://mini:8788"), &opts()).expect("pair"),
            AddMode::Manual {
                name: "mini".to_owned(),
                endpoint: "quic://mini:8788".to_owned()
            }
        );
        assert_eq!(
            AddMode::classify("ssh://me@box", None, &opts()).expect("uri"),
            AddMode::Manual {
                name: "box".to_owned(),
                endpoint: "ssh://me@box".to_owned()
            }
        );
        let named = AddOpts {
            name: Some("lab".to_owned()),
            ..opts()
        };
        assert_eq!(
            AddMode::classify("wss://10.0.0.5:8787", None, &named).expect("uri + --name"),
            AddMode::Manual {
                name: "lab".to_owned(),
                endpoint: "wss://10.0.0.5:8787".to_owned()
            }
        );
        assert_eq!(
            AddMode::classify("me@mini", None, &opts()).expect("ssh"),
            AddMode::Ssh {
                target: "me@mini".to_owned()
            }
        );
    }

    /// A flag from the other form is refused and the form it belongs to is
    /// named, rather than the flag being silently ignored.
    #[test]
    fn add_mode_refuses_flags_from_the_other_form() {
        let ssh_only = AddOpts {
            ssh_only: true,
            ..opts()
        };
        let err = AddMode::classify("mini", Some("ssh://mini"), &ssh_only)
            .expect_err("--ssh-only is an ssh-form flag");
        assert!(err.message.contains("--ssh-only"), "got {}", err.message);
        assert!(
            err.remedy.contains("phux host add HOST"),
            "got {}",
            err.remedy
        );

        let credentials = AddOpts {
            cert_fingerprint: Some("ab".repeat(32)),
            ..opts()
        };
        let err = AddMode::classify("me@mini", None, &credentials)
            .expect_err("credentials belong to the manual form");
        assert!(
            err.message.contains("--cert-fingerprint") && err.remedy.contains("quic://HOST:PORT"),
            "got {err:?}"
        );

        let named_pair = AddOpts {
            name: Some("x".to_owned()),
            ..opts()
        };
        assert!(
            AddMode::classify("mini", Some("ssh://mini"), &named_pair).is_err(),
            "--name and a NAME positional contradict"
        );
    }

    /// The three shared-middle failure classes map onto the error contract
    /// with distinct, pasteable remedies.
    #[test]
    fn add_failures_carry_pasteable_remedies() {
        let err = add_failure_error(
            "mini",
            HostRole::Remote,
            &EnrollFailure::SshUnreachable("ssh: connect to host mini port 22: refused".to_owned()),
        );
        assert_eq!(err.code, codes::REGISTRY);
        assert!(
            err.message.contains("cannot reach mini over ssh"),
            "got {}",
            err.message
        );
        assert!(
            err.remedy.contains("`ssh mini`")
                && err.remedy.contains("`phux host add mini --ssh-only`"),
            "got {}",
            err.remedy
        );

        let err = add_failure_error(
            "mini",
            HostRole::Remote,
            &EnrollFailure::MissingPhux("`phux` was not found on mini".to_owned()),
        );
        assert!(err.message.contains("no phux"), "got {}", err.message);
        assert!(
            err.remedy.contains("https://phux.sh/install") && err.remedy.contains("--remote-phux"),
            "the remedy names the install one-liner and the PATH escape: {}",
            err.remedy
        );

        let err = add_failure_error(
            "mini",
            HostRole::Remote,
            &EnrollFailure::Pair("remote `phux pair --json` reported no token".to_owned()),
        );
        assert!(
            err.message.contains("no token"),
            "the middle's message passes through: {}",
            err.message
        );
        assert!(
            err.remedy.contains("phux doctor")
                && err.remedy.contains("`phux host add mini --ssh-only`"),
            "got {}",
            err.remedy
        );
    }

    /// Under `--role satellite`, the pasteable remedy must carry the role,
    /// or following it verbatim would register into the remote registry.
    #[test]
    fn add_failure_remedies_carry_the_satellite_role() {
        let err = add_failure_error(
            "mini",
            HostRole::Satellite,
            &EnrollFailure::Pair("remote `phux pair --json` reported no token".to_owned()),
        );
        assert!(
            err.remedy
                .contains("`phux host add mini --role satellite --ssh-only`"),
            "got {}",
            err.remedy
        );
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
                ssh: Some("me@mini".to_owned()),
                direct: None,
                client_cert: None,
                client_key: None,
            },
            HostRow {
                name: "edge".to_owned(),
                role: HostRole::Satellite,
                endpoint: "ssh://edge".to_owned(),
                enabled: Some(false),
                token_file: None,
                cert_fingerprint: None,
                session: None,
                ssh: None,
                direct: None,
                client_cert: None,
                client_key: None,
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

    /// The empty states teach the one command that fills the table — the
    /// ssh form, with the role flag under a satellite filter.
    #[test]
    fn empty_states_name_the_add_command() {
        let merged = empty_state(None);
        assert!(merged.contains("phux host add me@HOST"));
        assert!(merged.contains("--role satellite"));

        let remotes = empty_state(Some(HostRole::Remote));
        assert!(remotes.contains("phux host add me@HOST"));
        assert!(!remotes.contains("--role satellite"));

        let satellites = empty_state(Some(HostRole::Satellite));
        assert!(satellites.contains("--role satellite"));
    }

    /// Regression: a name escaping `<state>/remotes` (`../../evil`) must be
    /// rejected before any token is written.
    #[test]
    fn enroll_rejects_a_traversal_name_before_writing_any_token() {
        let state_dir = tempfile::tempdir().expect("tempdir");

        let pairing = super::enroll::PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: Some("ab".repeat(32)),
            overlay_addresses: vec!["100.64.0.2".to_owned()],
        };
        let route = Route {
            pairing: Some(&pairing),
            ..Route::bare("quic://mini:8788")
        };
        let err = finish_enroll_in(
            state_dir.path(),
            HostRole::Remote,
            "../../evil",
            &route,
            None,
            Some("me@mini"),
        )
        .expect_err("a traversal name must be refused");
        assert_eq!(err.code, codes::REGISTRY);

        // Nothing that looks like a bearer token landed anywhere under the
        // (fake) state dir — not at the intended path, and not at the
        // traversal target either.
        let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
        assert!(
            found.is_empty(),
            "rejection must not leave an orphaned token file: {found:?}"
        );
    }

    /// Regression: an unpinned `quic://` endpoint fails before `write_token`,
    /// leaving no orphaned bearer token.
    #[test]
    fn enroll_rejects_unpinned_quic_before_writing_any_token() {
        let state_dir = tempfile::tempdir().expect("tempdir");

        let pairing = super::enroll::PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: None,
            overlay_addresses: vec!["100.64.0.2".to_owned()],
        };
        let route = Route {
            pairing: Some(&pairing),
            ..Route::bare("quic://mini:8788")
        };
        let err = finish_enroll_in(
            state_dir.path(),
            HostRole::Remote,
            "mini",
            &route,
            None,
            Some("me@mini"),
        )
        .expect_err("an unpinned quic endpoint must be refused");
        assert!(err.message.contains("--cert-fingerprint"), "got {err:?}");

        let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
        assert!(
            found.is_empty(),
            "rejection must not leave an orphaned token file: {found:?}"
        );
    }

    /// An `ssh://` route with no direct candidate keeps no credential; one with a
    /// candidate keeps token and pin; a satellite never keeps any beside `ssh://`.
    #[test]
    fn ssh_route_keeps_credentials_only_for_a_direct_candidate() {
        assert!(!keeps_credentials(HostRole::Remote, "ssh://me@mini", None));
        assert!(keeps_credentials(
            HostRole::Remote,
            "ssh://me@mini",
            Some("quic://100.64.0.2:8788")
        ));
        assert!(keeps_credentials(
            HostRole::Remote,
            "quic://mini:8788",
            None
        ));
        assert!(!keeps_credentials(
            HostRole::Satellite,
            "ssh://edge",
            Some("quic://100.64.0.2:8788")
        ));
        assert!(keeps_credentials(
            HostRole::Satellite,
            "quic://edge:8788",
            None
        ));
    }

    /// Every `*.token` file under `root`, walked without pulling in a full
    /// directory-walking crate for two tests.
    fn walkdir_tokens(root: &Path) -> impl Iterator<Item = PathBuf> + use<> {
        fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    visit(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "token") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        visit(root, &mut out);
        out.into_iter()
    }
}
