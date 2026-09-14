//! `phux host` — one visible namespace over the two machine registries
//! (ADR-0066, ADR-0122).
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
//! `phux host add` is the one front door (ADR-0122). Given `[USER@]HOST` it
//! sets the machine up over ssh end to end — confirms phux is there, starts
//! and supervises its server, pairs, dials the direct routes, and registers
//! the first one that answers ([`super::enroll::enroll_over_ssh`]). Given a
//! `NAME ENDPOINT` pair or an endpoint URI it registers what it is told,
//! which is the form for credentials minted elsewhere. `phux host enroll`
//! was the ssh form's old spelling and survives one release cycle as a
//! hidden alias.
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
//!       "direct": null
//!     }
//!   ]
//! }
//! ```
//!
//! `enabled` is `null` for remotes (the schema has no enabled bit); `session`,
//! `ssh`, and `direct` are `null` for satellites (a hub-dialed link has no
//! arrival to attach and is never repaired over ssh). `ssh` and `direct`
//! joined the document under the same `schema_version`: readers tolerate
//! keys they do not know, so two more are not a break. `host add --json`
//! wraps one such object under `"host"`; `host rm --json` emits
//! `{"schema_version":1,"removed":{"name":..,"role":..}}`. Failures follow
//! the shared JSON error contract in [`super::json_err`].

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use usage::{Args, Subcommands, ValueEnum};

use super::JsonOpt;
use super::enroll::{self, EnrollEvent, EnrollFailure, EnrollRequest, ServicePolicy};
use super::json_err::{self, CliError, codes};
use super::remote::{self, Endpoint, RemoteEntry};
use super::remote_target::RemoteTarget;
use super::satellite::registry as satellite_registry;
use super::service;

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

    /// The `--role` flag to paste into a remedy, empty for the default.
    const fn flag(self) -> &'static str {
        match self {
            Self::Remote => "",
            Self::Satellite => " --role satellite",
        }
    }
}

/// The flags `phux host add` takes, shared with its hidden `enroll`
/// spelling so the two cannot drift.
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

    /// Former spelling of `phux host add HOST`.
    #[usage(hide)]
    Enroll {
        /// ssh destination, exactly as you would type it after `ssh`.
        host: String,

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
}

/// The old spelling's deprecation row, whose `note` is the one line the
/// alias prints. Read from the table rather than restated so the audit,
/// the generated page, and the warning cannot disagree.
fn enroll_deprecation_note() -> &'static str {
    crate::deprecations::DEPRECATED
        .iter()
        .find(|row| row.old == "phux host enroll")
        .map_or(
            "phux: `phux host enroll` is deprecated and will be removed; use `phux host add`",
            |row| row.note,
        )
}

pub(crate) fn run_host(action: &HostAction) -> ExitCode {
    match action {
        HostAction::Add {
            target,
            endpoint,
            opts,
        } => run_add(target, endpoint.as_deref(), opts),
        HostAction::Enroll { host, opts } => {
            // Under `--json` stdout is the document and stderr the one-line
            // error contract, so the note is suppressed on both.
            if !opts.json.json {
                eprintln!("{}", enroll_deprecation_note());
            }
            run_add(host, None, opts)
        }
        HostAction::List { role, json } => run_list(*role, json.json),
        HostAction::Remove { name, role, json } => run_remove(name, *role, json.json),
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
        }
    }
}

/// Refuse a flag paired with the role it does not apply to, naming the
/// remedy. Returns `None` when the pairing is coherent.
///
/// Enforced post-parse because the parser cannot make a flag's validity
/// depend on another flag's *value* — and a silent ignore would be worse
/// than either.
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
    /// `NAME ENDPOINT` and a bare endpoint URI are the manual form;
    /// anything else is an ssh destination. A flag that belongs to the
    /// other form is refused, with the form it belongs to named, rather
    /// than silently ignored.
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

/// The host an endpoint URI addresses, for the default name of a bare-URI
/// add.
fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split_once(']').map(|(host, _)| host)?
    } else if authority.matches(':').count() > 1 {
        authority
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_owned())
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
    let ssh_host = target.ssh_destination();
    let name = opts.name.clone().unwrap_or_else(|| target.host.clone());
    let quic_port = target.port.unwrap_or(opts.quic_port);

    // `--ssh-only` skips the host entirely: no pairing, no service, just a
    // registry entry riding existing ssh trust.
    if opts.ssh_only {
        let endpoint = format!("ssh://{ssh_host}");
        let registered = finish_enroll(
            opts.role,
            &name,
            &endpoint,
            None,
            opts.session.as_deref(),
            Some(&ssh_host),
            None,
        );
        return match registered {
            Ok(row) => report_registered(
                &row,
                "rides your ssh trust; nothing on the host was touched",
                &[],
                json,
                local_hub_for(opts.role).as_ref(),
            ),
            Err(err) => json_err::emit(json, &err, 1),
        };
    }

    let narrate = |event: &EnrollEvent| {
        if !json {
            eprintln!("{name}: {}", event.describe());
        }
    };

    // Already registered and answering: say so and stop, rather than
    // re-pairing a machine that needs nothing.
    if opts.role == HostRole::Remote
        && let Some(entry) = remote::find(&name)
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
    };
    let outcome = match enroll::enroll_over_ssh(&req, &mut |event| narrate(&event)) {
        Ok(outcome) => outcome,
        Err(failure) => {
            return json_err::emit(json, &add_failure_error(&ssh_host, opts.role, &failure), 1);
        }
    };

    let registered = finish_enroll(
        opts.role,
        &name,
        &outcome.endpoint,
        Some(&outcome.report),
        opts.session.as_deref(),
        Some(&ssh_host),
        outcome.direct.as_deref(),
    );
    match registered {
        Ok(row) => report_ssh_add(&row, &outcome, &ssh_host, quic_port, opts),
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
    Some(enroll::probe(
        &target,
        &token,
        entry.cert_fingerprint.as_deref(),
    ))
}

/// Set a registered remote up over ssh and rewrite its entry: the attach
/// repair rung's way in, so `phux --remote` and `phux host add` register
/// through one tail.
///
/// `narrate` receives each progress event; the caller prefixes it for its
/// own output contract.
pub(crate) fn enroll_remote_over_ssh(
    name: &str,
    req: &EnrollRequest<'_>,
    session: Option<&str>,
    narrate: &mut dyn FnMut(&EnrollEvent),
) -> Result<(RemoteEntry, enroll::EnrollOutcome), CliError> {
    let outcome = enroll::enroll_over_ssh(req, &mut |event| narrate(&event))
        .map_err(|failure| add_failure_error(req.ssh_host, HostRole::Remote, &failure))?;
    finish_enroll(
        HostRole::Remote,
        name,
        &outcome.endpoint,
        Some(&outcome.report),
        session,
        Some(req.ssh_host),
        outcome.direct.as_deref(),
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

/// The role-specific tail of the ssh form: validate the entry, write the
/// pairing token under the role-correct directory (`remotes/` vs
/// `satellites/`), and register into the matching registry. `pairing` is
/// `None` on the `--ssh-only` path.
///
/// Validation runs BEFORE the token hits disk: a rejected name (`../x`
/// would escape the token directory via its join) or a quic endpoint
/// missing its fingerprint must not leave an orphaned bearer token behind.
fn finish_enroll(
    role: HostRole,
    name: &str,
    endpoint: &str,
    pairing: Option<&enroll::PairReport>,
    session: Option<&str>,
    ssh: Option<&str>,
    direct: Option<&str>,
) -> Result<HostRow, CliError> {
    finish_enroll_in(
        &phux_server::telemetry::state_dir(),
        role,
        name,
        endpoint,
        pairing,
        session,
        ssh,
        direct,
    )
}

/// [`finish_enroll`] with the state directory injectable, so a test can
/// drive the validate-before-write ordering (phux-522) against a tempdir
/// instead of the operator's real `$XDG_STATE_HOME` — this crate forbids
/// `unsafe`, so `env::set_var` (unsafe under edition 2024) is not an option
/// for pointing `phux_server::telemetry::state_dir()` elsewhere.
#[allow(
    clippy::too_many_arguments,
    reason = "the tail takes every field of the entry it writes; a struct would only rename the list"
)]
fn finish_enroll_in(
    state_dir: &Path,
    role: HostRole,
    name: &str,
    endpoint: &str,
    pairing: Option<&enroll::PairReport>,
    session: Option<&str>,
    ssh: Option<&str>,
    direct: Option<&str>,
) -> Result<HostRow, CliError> {
    let direct = direct.filter(|_| role == HostRole::Remote);
    let dialable = keeps_credentials(role, endpoint, direct);
    let token_file = (pairing.is_some() && dialable).then(|| match role {
        HostRole::Remote => enroll::token_path(state_dir, name),
        HostRole::Satellite => enroll::satellite_token_path(state_dir, name),
    });
    let cert_fingerprint = pairing
        .filter(|_| dialable)
        .and_then(|report| report.cert_fingerprint.as_deref());

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

/// Whether an enrolled entry stores its pairing token and pin.
///
/// The token is only meaningful for a dialed transport: an `ssh://` entry
/// rides ssh trust and must not leave a stray credential on disk — unless
/// it keeps a direct route to promote, which the token is for. Only the
/// remote schema has a `direct` key; a satellite is never promoted.
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
/// human summary otherwise — what was registered, how it connects, and the
/// three commands worth knowing next.
///
/// `hub` is `Some` only for `--role satellite`, where the local unit was
/// made a hub (or skipped, with the reason on stderr).
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
        return print_doc(&doc);
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
            outln!("  {:<28}{transport}", format!("phux attach {name}"));
            outln!(
                "  {:<28}list its sessions",
                format!("phux ls --remote {name}")
            );
            outln!("  {:<28}every registered machine", "phux host ls");
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
    use std::path::{Path, PathBuf};

    use super::{
        AddMode, AddOpts, HostRole, HostRow, add_failure_error, auth_display, empty_state,
        endpoint_host, finish_enroll_in, keeps_credentials, render_table, resolve_rm_role,
        role_flag_mismatch, sort_rows,
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
        }
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

    #[test]
    fn endpoint_host_reads_every_registry_scheme() {
        assert_eq!(endpoint_host("quic://mini:8788").as_deref(), Some("mini"));
        assert_eq!(
            endpoint_host("wss://me@mini.ts.net:8787").as_deref(),
            Some("mini.ts.net")
        );
        assert_eq!(endpoint_host("ssh://mini").as_deref(), Some("mini"));
        assert_eq!(
            endpoint_host("quic://[fd7a::1]:8788").as_deref(),
            Some("fd7a::1")
        );
        assert_eq!(endpoint_host("not-a-uri"), None);
        assert_eq!(endpoint_host("quic://"), None);
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

    /// phux-522 regression: a name that would escape `<state>/remotes` via
    /// `token_path`'s naive `join` (e.g. `../../evil`) must be rejected by
    /// `NewRemote::new` before `finish_enroll` ever calls `write_token` — a
    /// token written first and rejected second leaves a live bearer
    /// credential sitting outside the registry's control.
    ///
    /// Drives [`finish_enroll_in`] against a tempdir rather than mutating
    /// `$XDG_STATE_HOME` (this crate forbids `unsafe`, and `env::set_var`
    /// is unsafe under edition 2024).
    #[test]
    fn enroll_rejects_a_traversal_name_before_writing_any_token() {
        let state_dir = tempfile::tempdir().expect("tempdir");

        let pairing = super::enroll::PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: Some("ab".repeat(32)),
            overlay_addresses: vec!["100.64.0.2".to_owned()],
        };
        let err = finish_enroll_in(
            state_dir.path(),
            HostRole::Remote,
            "../../evil",
            "quic://mini:8788",
            Some(&pairing),
            None,
            Some("me@mini"),
            None,
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

    /// phux-522 regression: a `quic://` endpoint with no `--cert-fingerprint`
    /// fails `NewRemote::new`'s pin requirement (ADR-0038). That failure must
    /// happen before `write_token`, or every rejected unpinned enrollment
    /// leaves a 0600 bearer token on disk that nothing ever points at.
    #[test]
    fn enroll_rejects_unpinned_quic_before_writing_any_token() {
        let state_dir = tempfile::tempdir().expect("tempdir");

        let pairing = super::enroll::PairReport {
            token: "deadbeef".to_owned(),
            cert_fingerprint: None,
            overlay_addresses: vec!["100.64.0.2".to_owned()],
        };
        let err = finish_enroll_in(
            state_dir.path(),
            HostRole::Remote,
            "mini",
            "quic://mini:8788",
            Some(&pairing),
            None,
            Some("me@mini"),
            None,
        )
        .expect_err("an unpinned quic endpoint must be refused");
        assert!(err.message.contains("--cert-fingerprint"), "got {err:?}");

        let found: Vec<_> = walkdir_tokens(state_dir.path()).collect();
        assert!(
            found.is_empty(),
            "rejection must not leave an orphaned token file: {found:?}"
        );
    }

    /// An `ssh://` route with no direct candidate leaves no credential
    /// behind; one that keeps a candidate to promote stores the token and
    /// the pin it will need; a satellite is never promoted, so it keeps
    /// nothing beside an `ssh://` route. Pure on purpose: the tail that
    /// acts on this writes the operator's real registry, so the binary-level
    /// tests in `tests/fleet/host_enroll.rs` cover the write under private
    /// config and state dirs.
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
