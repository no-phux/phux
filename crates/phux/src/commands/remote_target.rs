//! `phux --remote [USER@]HOST[:PORT]` — the one-shot remote front door: like
//! `ssh user@host`, no prior client setup. The resolution ladder, cheapest
//! first:
//!
//! 1. a registered `[[remote]]` entry: a direct QUIC dial, no ssh;
//! 2. a pasted `--code` connect link: register from the link and dial;
//! 3. a one-time ssh bootstrap: install the far end's service, run `phux pair`
//!    over ssh, register, and dial (rung 1 catches every later invocation);
//! 4. a refusal naming both remedies.
//!
//! A registered host that stops answering is repaired here too ([`run`]):
//! start its server over ssh, then re-pair, then report both errors. `phux
//! attach NAME` reaches the same ladder through [`run_registered`], and `phux
//! host add HOST` registers through the same tail (ADR-0122).
//!
//! `user@` is a label for the ssh destination and registry key, not a wire
//! identity: the server reached is decided by address and port (ADR-0003).
//! `--no-enroll` is the no-ssh boundary; a pasted code never touches the host.

mod code;
mod quic_upgrade;

pub(crate) use code::pair_from_code;

use std::process::ExitCode;

use super::attach;
use super::enroll;
use super::host;
use super::json_err::CliError;
use super::rec::RecordSpec;
use super::remote::{self, Endpoint, RemoteEntry};

pub(crate) use phux_client_runtime::target::{
    DEFAULT_QUIC_PORT, RemoteTarget, endpoint_host, endpoint_port,
};
use quic_upgrade::promote_direct_route;
pub(crate) use quic_upgrade::{QuicUpgrade, upgrade_to_quic};

/// Parse a `--remote` target: `host`, `user@host`, `host:port`,
/// `user@host:port`, and bracketed IPv6. The grammar is the runtime's (one
/// for the CLI and every embedder); errors name `--remote`.
pub(crate) fn parse_target(raw: &str) -> Result<RemoteTarget, String> {
    RemoteTarget::parse_labeled(raw, "--remote")
}

/// Find the registry entry for this target: a typed port's `host-PORT`, the
/// exact `user@host`, the bare host, then any entry whose endpoint points at
/// the host (at the typed port, when one was typed). An unreadable
/// config yields `None`, falling through to the bootstrap rungs.
pub(crate) fn find_entry(target: &RemoteTarget) -> Option<RemoteEntry> {
    let entries = remote::load_registry().ok()?;
    phux_client_runtime::target::find_entry_by(&entries, target, |entry| {
        (&entry.name, &entry.endpoint)
    })
    .cloned()
}

/// The name a new `role` entry for `base` typed with `port` is saved under:
/// `base`, or `base-PORT` when `base` is another server's at another port.
pub(crate) fn registration_name(role: host::HostRole, base: &str, port: Option<u16>) -> String {
    let entries: Vec<(String, String)> = match role {
        host::HostRole::Remote => remote::load_registry()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.name, entry.endpoint))
            .collect(),
        host::HostRole::Satellite => super::satellite::load_registry()
            .unwrap_or_default()
            .into_iter()
            .map(|entry| (entry.name, entry.endpoint))
            .collect(),
    };
    phux_client_runtime::target::registration_name_by(&entries, base, port, |(name, endpoint)| {
        (name, endpoint)
    })
}

/// Apply an explicit `:PORT` to a registered entry for this dial only; the
/// config is untouched and `ssh://` has no port.
pub(crate) fn with_port_override(entry: RemoteEntry, target: &RemoteTarget) -> RemoteEntry {
    if target.port.is_none() {
        return entry;
    }
    let Ok(parsed) = Endpoint::parse(&entry.endpoint) else {
        return entry;
    };
    let endpoint = match parsed {
        Endpoint::Quic(_) => format!("quic://{}", target.authority()),
        Endpoint::Ws(url) => {
            let scheme = if url.starts_with("ws://") {
                "ws"
            } else {
                "wss"
            };
            format!("{scheme}://{}", target.authority())
        }
        // ssh:// has no port in this grammar; leave the entry alone rather
        // than fabricating one the ssh config may already answer.
        Endpoint::Ssh(_) => return entry,
    };
    RemoteEntry { endpoint, ..entry }
}

/// How a cold target may be bootstrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bootstrap {
    /// Try the ssh rung when no code was pasted. The default.
    Auto,
    /// Never shell out to ssh: refuse instead, naming the remedies.
    Never,
}

/// Everything `phux --remote` was invoked with.
pub(crate) struct RemoteAttach<'a> {
    /// The parsed target.
    pub(crate) target: RemoteTarget,
    /// A session name to request on arrival, overriding the entry's own.
    pub(crate) session: Option<String>,
    /// A pasted `https://phux.sh/connect?...` code (or its
    /// `phux://connect?...` spelling), for the ssh-free cold path.
    pub(crate) code: Option<&'a str>,
    /// Whether a cold target may bootstrap over ssh.
    pub(crate) bootstrap: Bootstrap,
    /// Recording spec for the attach that follows.
    pub(crate) rec: Option<&'a RecordSpec>,
}

/// `phux attach NAME` for a registered `NAME`: the same ladder as
/// `phux --remote NAME`, entered from the entry the registry already
/// resolved.
pub(crate) fn run_registered(name: &str, entry: RemoteEntry, rec: Option<&RecordSpec>) -> ExitCode {
    if let Err(paused) = refuse_paused(&entry) {
        crate::output::error_block(paused.lines());
        return ExitCode::FAILURE;
    }
    // A registry name is validated (no sigil, no `/`, no `:`), so it always
    // parses as a bare host; the entry itself is what gets dialed.
    let target = parse_target(name).unwrap_or_else(|_| RemoteTarget {
        user: None,
        host: name.to_owned(),
        port: None,
    });
    attach_registered(
        &RemoteAttach {
            target,
            session: None,
            code: None,
            bootstrap: Bootstrap::Auto,
            rec,
        },
        entry,
    )
}

/// A paused host is kept but never dialed (`phux host disable`).
fn refuse_paused(entry: &RemoteEntry) -> Result<(), String> {
    if entry.paused {
        return Err(format!(
            "{0} is paused\nrun `phux host enable {0}` to use it again",
            entry.name
        ));
    }
    Ok(())
}

/// Resolve a `--remote` target to a registered host and attach to it.
pub(crate) fn run(args: RemoteAttach<'_>) -> ExitCode {
    let was_registered = args.code.is_none() && find_entry(&args.target).is_some();
    let entry = match resolve(&args.target, args.code, args.bootstrap) {
        Ok(entry) => entry,
        Err(err) => {
            crate::output::error_block(err.report.lines());
            return ExitCode::from(err.exit);
        }
    };
    if was_registered {
        return attach_registered(&args, entry);
    }
    // A pasted code registers its `wss://` url with the link's QUIC route
    // beside it; move onto QUIC now if it answers. The ssh rung already
    // tried its direct routes.
    let entry = if args.code.is_some() {
        promote_direct_route(entry)
    } else {
        entry
    };
    // Just paired: the dial is the proof, and a failure now is reported
    // as itself rather than repaired against credentials seconds old.
    attach::run_attach_remote(&entry, args.session, args.rec)
}

/// Attach through a registered entry, repairing it over ssh on an early
/// failure: dial the saved route (a kept `direct` first); if nobody answered,
/// start the server over ssh and redial; if still refused, re-pair and redial;
/// if ssh itself failed, report both. `--no-enroll` stops after the first dial.
fn attach_registered(args: &RemoteAttach<'_>, entry: RemoteEntry) -> ExitCode {
    // A typed `:PORT` overrides this dial only; it is not the place to
    // rewrite the entry's route.
    let entry = if args.target.port.is_some() {
        entry
    } else {
        promote_direct_route(entry)
    };
    let outcome = attach::run_attach_remote_outcome(&entry, args.session.clone(), args.rec);
    if args.bootstrap == Bootstrap::Never {
        return outcome.code;
    }
    let name = entry.name.clone();
    // The destination the entry was set up through; failing that, the
    // `user@host` the operator typed (an ssh alias with a user survives),
    // and only then the entry's own reading of itself.
    let ssh_host = entry.ssh.clone().unwrap_or_else(|| {
        if args.target.user.is_some() {
            args.target.registry_name()
        } else {
            entry.ssh_destination()
        }
    });
    // Only a `quic://` endpoint names the QUIC port; a `wss://` one names
    // the WebSocket listener's.
    let quic_port = Some(entry.endpoint.as_str())
        .filter(|endpoint| endpoint.starts_with("quic://"))
        .and_then(endpoint_port)
        .unwrap_or(DEFAULT_QUIC_PORT);
    match outcome.repair {
        attach::Repair::None => return outcome.code,
        attach::Repair::Start => {
            info!(
                "{name} is not answering at {}; starting its server over ssh ({ssh_host})...",
                entry.endpoint
            );
            match enroll::ensure_remote_server(
                &ssh_host,
                entry.remote_phux(),
                quic_port,
                enroll::ServicePolicy::Install,
            ) {
                Ok(supervision) => {
                    crate::output::diagnostic(
                        supervision.severity(),
                        format_args!("{name}: {}", supervision.describe()),
                    );
                    let again =
                        attach::run_attach_remote_outcome(&entry, args.session.clone(), args.rec);
                    if again.repair == attach::Repair::None {
                        return again.code;
                    }
                    info!(
                        "{name} still does not answer with the saved credentials; re-pairing over ssh..."
                    );
                }
                Err(failure) => {
                    error!("{name}: could not start it over ssh: {}", failure.detail());
                    hint!(
                        "check `ssh {ssh_host}` from this machine, then `phux attach {name}` again; \
                         or, if the server should be up, run `phux doctor` on {name}"
                    );
                    return ExitCode::from(failure.exit_code());
                }
            }
        }
        attach::Repair::RePair => {
            info!(
                "{name} refused the saved route ({}); re-pairing over ssh ({ssh_host})...",
                entry.endpoint
            );
        }
    }
    let entry = match register_over_ssh(&args.target, &ssh_host, Some(&entry)) {
        Ok(entry) => entry,
        Err(err) => {
            error!("{name}: {}", err.message);
            hint!("{}", err.remedy);
            return ExitCode::FAILURE;
        }
    };
    attach::run_attach_remote(&entry, args.session.clone(), args.rec)
}

/// Why a target did not resolve, and the exit code that says so.
pub(crate) struct Unresolved {
    pub(crate) report: String,
    pub(crate) exit: u8,
}

impl From<String> for Unresolved {
    fn from(report: String) -> Self {
        Self {
            report,
            exit: crate::exit_codes::EXIT_FAILURE,
        }
    }
}

/// Walk the ladder: registered, pasted code, ssh, refuse. Shared with the
/// headless verbs' `--remote` so a target means one host everywhere.
pub(crate) fn resolve(
    target: &RemoteTarget,
    code: Option<&str>,
    bootstrap: Bootstrap,
) -> Result<RemoteEntry, Unresolved> {
    // Rung 1: already registered. A pasted `--code` still wins, because the
    // operator is holding fresher credentials than the ones on disk — that
    // is what re-pairing after a revoke looks like.
    if code.is_none()
        && let Some(entry) = find_entry(target)
    {
        refuse_paused(&entry)?;
        return Ok(with_port_override(entry, target));
    }

    // Rung 2: a connect code carries the endpoint, the pin, and the token.
    if let Some(code) = code {
        return code::register_from_code(target, code).map_err(Unresolved::from);
    }

    // Rung 3: mint credentials over the operator's existing ssh trust.
    if bootstrap == Bootstrap::Never {
        return Err(unregistered_message(target, None).into());
    }
    // `ssh` takes the typed `user@host` spelling as its destination.
    register_over_ssh(target, &target.registry_name(), None).map_err(|err| Unresolved {
        exit: if err.message.starts_with("cannot reach ") {
            crate::exit_codes::ssh_exit(&err.message)
        } else {
            crate::exit_codes::EXIT_FAILURE
        },
        report: unregistered_message(target, Some(&format!("{}\n{}", err.message, err.remedy))),
    })
}

/// Set the far end up over ssh, register the result, and return the entry to
/// dial. A repaired entry keeps its name; a cold target registers the typed
/// spelling.
fn register_over_ssh(
    target: &RemoteTarget,
    ssh_host: &str,
    existing: Option<&RemoteEntry>,
) -> Result<RemoteEntry, CliError> {
    let name = existing.map_or_else(|| target.registry_name(), |entry| entry.name.clone());
    info!("setting {} up over ssh ({ssh_host})...", target.host);

    // The target's own authority when a port was given, so an operator who
    // knows their listener is not on 8788 does not have to enroll
    // separately to say so.
    let endpoint_override = target.port.map(|_| target.authority());
    let previous = existing
        .map(host::PreviousEnrollment::of)
        .unwrap_or_default();
    let remotes_dir = phux_config::instance::state_dir().join("remotes");
    let req = enroll::EnrollRequest {
        ssh_host,
        remote_phux: existing.map_or("phux", RemoteEntry::remote_phux),
        endpoint_override: endpoint_override.as_deref(),
        quic_port: target.port.unwrap_or(DEFAULT_QUIC_PORT),
        service: enroll::ServicePolicy::Install,
        previous_token: previous.token.as_deref(),
        previous_fingerprint: previous.fingerprint.as_deref(),
        previous_identity: previous.identity.as_ref(),
        workload: Some(previous.held.replacement(&remotes_dir, &name, false)),
    };
    let (entry, outcome) = host::enroll_remote_over_ssh(
        &name,
        &req,
        existing.and_then(|entry| entry.session.as_deref()),
        &previous.held,
        &mut |event| event.report_progress(&name),
    )?;

    ok!("registered {name} -> {}", entry.endpoint);
    if entry.endpoint.starts_with("ssh://") {
        info!(
            "no direct route answered{}; this attach rides ssh, and every attach tries the direct route first",
            if outcome.tried.is_empty() {
                String::new()
            } else {
                format!(" (tried {})", outcome.tried.join(", "))
            }
        );
    } else {
        info!("later attaches dial it directly; ssh is out of the path");
    }
    Ok(entry)
}

/// What to print when a target cannot be resolved: the two remedies, in the
/// order an operator can act on them.
fn unregistered_message(target: &RemoteTarget, ssh_error: Option<&str>) -> String {
    use std::fmt::Write as _;

    let name = target.registry_name();
    let mut message = format!("{name} is not a registered host");
    if let Some(err) = ssh_error {
        // `write!` into a String is infallible; the Result is discarded
        // rather than unwrapped so no error path exists to mishandle.
        let _ = write!(message, " and setting it up over ssh failed:\n{err}");
    }
    let _ = write!(message, "\n{}", unregistered_remedies(target));
    message
}

/// The two remedies for an unregistered host, in the order an operator can
/// act on them.
fn unregistered_remedies(target: &RemoteTarget) -> String {
    let name = target.registry_name();
    format!(
        "to pair without ssh, run `phux pair` on {} and paste the link:\
         \nphux attach --remote {name} --code '<https://phux.sh/connect?...>'\
         \nor, with ssh access, `phux host add {name}` (starts and supervises the server there)",
        target.host
    )
}

#[cfg(test)]
mod tests;
