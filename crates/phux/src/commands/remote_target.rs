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

use std::process::ExitCode;

use super::attach;
use super::enroll;
use super::host;
use super::json_err::CliError;
use super::pair;
use super::rec::RecordSpec;
use super::remote::{self, Endpoint, RemoteEntry};

/// The default QUIC port a server auto-binds (ADR-0081), and therefore the
/// port a `--remote` target with no `:PORT` means.
const DEFAULT_QUIC_PORT: u16 = 8788;

/// A parsed `[USER@]HOST[:PORT]` remote target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteTarget {
    /// The `user@` half, when the operator typed one. Names the ssh
    /// destination for the bootstrap rung; never travels on the wire.
    pub(crate) user: Option<String>,
    /// Hostname or IP literal. IPv6 is stored unbracketed; [`Self::authority`]
    /// re-brackets it.
    pub(crate) host: String,
    /// An explicit `:PORT`, which overrides whatever a registry entry
    /// remembers for this dial only.
    pub(crate) port: Option<u16>,
}

impl RemoteTarget {
    /// Parse `host`, `user@host`, `host:port`, `user@host:port`, and bracketed
    /// IPv6. A URI is refused with a pointer to `phux host add`.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        Self::parse_labeled(raw, "--remote")
    }

    /// [`Self::parse`] with the errors worded for another spelling of the
    /// same grammar (`phux host add HOST` shares it).
    pub(crate) fn parse_labeled(raw: &str, label: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(format!("{label} needs a target, e.g. {label} me@mini"));
        }
        if trimmed.contains("://") {
            return Err(format!(
                "{label} takes [USER@]HOST[:PORT], not a URI (got {trimmed:?}); \
                 register a full endpoint with `phux host add NAME {trimmed}`"
            ));
        }

        // Split on the LAST `@`: a username cannot contain `@`, but this way
        // an address that does is still parsed the way the operator meant.
        let (user, rest) = match trimmed.rsplit_once('@') {
            Some((user, rest)) => {
                if user.is_empty() {
                    return Err(format!("{label} target {trimmed:?} has an empty user"));
                }
                (Some(user.to_owned()), rest)
            }
            None => (None, trimmed),
        };

        let (host, port) = split_host_port(rest, label)?;
        if host.is_empty() {
            return Err(format!("{label} target {trimmed:?} has an empty host"));
        }
        // A registry name may not contain `/` (it would escape the token
        // directory on join) or a selector sigil. Catch it here, where the
        // message can point at what the operator typed.
        if host.contains('/') || host.starts_with(['@', '#', '.', '=']) {
            return Err(format!(
                "{label} host {host:?} must not contain '/' or start with a selector sigil (@ # . =)"
            ));
        }

        Ok(Self { user, host, port })
    }

    /// The registry key: the typed spelling minus any port, so `mini` and
    /// `mini:8788` are one host.
    pub(crate) fn registry_name(&self) -> String {
        self.user
            .as_ref()
            .map_or_else(|| self.host.clone(), |user| format!("{user}@{}", self.host))
    }

    /// The `HOST:PORT` authority to dial, bracketing an IPv6 literal.
    pub(crate) fn authority(&self) -> String {
        let port = self.port.unwrap_or(DEFAULT_QUIC_PORT);
        if self.host.contains(':') {
            format!("[{}]:{port}", self.host)
        } else {
            format!("{}:{port}", self.host)
        }
    }

    /// The destination to hand `ssh`, which understands `user@host` natively.
    pub(crate) fn ssh_destination(&self) -> String {
        self.registry_name()
    }
}

/// Split `HOST[:PORT]`, honoring `[v6]:port` and a bare IPv6 literal.
fn split_host_port(rest: &str, label: &str) -> Result<(String, Option<u16>), String> {
    if let Some(inner) = rest.strip_prefix('[') {
        let (host, tail) = inner
            .split_once(']')
            .ok_or_else(|| format!("{label} target {rest:?} has an unclosed '['"))?;
        let port = match tail {
            "" => None,
            tail => Some(parse_port(
                tail.strip_prefix(':').ok_or_else(|| {
                    format!("{label} target {rest:?} has trailing text after ']'")
                })?,
                label,
            )?),
        };
        return Ok((host.to_owned(), port));
    }
    // More than one `:` and no brackets means a bare IPv6 literal: it has no
    // port, because `fd7a::1:8788` is ambiguous and guessing would silently
    // dial the wrong address.
    if rest.matches(':').count() > 1 {
        return Ok((rest.to_owned(), None));
    }
    match rest.split_once(':') {
        Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port, label)?))),
        None => Ok((rest.to_owned(), None)),
    }
}

fn parse_port(raw: &str, label: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("{label} port {raw:?} must be 1..=65535"))
}

/// Find the registry entry for this target: the exact `user@host`, then the
/// bare host, then any entry whose endpoint points at the host. An unreadable
/// config yields `None`, falling through to the bootstrap rungs.
pub(crate) fn find_entry(target: &RemoteTarget) -> Option<RemoteEntry> {
    let entries = remote::load_registry().ok()?;
    let name = target.registry_name();
    entries
        .iter()
        .find(|entry| entry.name == name)
        .or_else(|| entries.iter().find(|entry| entry.name == target.host))
        .or_else(|| {
            entries
                .iter()
                .find(|entry| endpoint_host(&entry.endpoint).as_deref() == Some(&target.host))
        })
        .cloned()
}

/// The host an endpoint URI addresses, for the third match above.
fn endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    if let Some(inner) = authority.strip_prefix('[') {
        return inner.split_once(']').map(|(host, _)| host.to_owned());
    }
    if authority.matches(':').count() > 1 {
        return Some(authority.to_owned());
    }
    Some(
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
            .to_owned(),
    )
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
    // A registry name is validated (no sigil, no `/`, no `:`), so it always
    // parses as a bare host; the entry itself is what gets dialed.
    let target = RemoteTarget::parse(name).unwrap_or_else(|_| RemoteTarget {
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

/// Resolve a `--remote` target to a registered host and attach to it.
pub(crate) fn run(args: RemoteAttach<'_>) -> ExitCode {
    let was_registered = args.code.is_none() && find_entry(&args.target).is_some();
    let entry = match resolve(&args.target, args.code, args.bootstrap) {
        Ok(entry) => entry,
        Err(err) => {
            for line in err.lines() {
                eprintln!("{line}");
            }
            return ExitCode::FAILURE;
        }
    };
    if was_registered {
        attach_registered(&args, entry)
    } else {
        // Just paired: the dial is the proof, and a failure now is reported
        // as itself rather than repaired against credentials seconds old.
        attach::run_attach_remote(&entry, args.session, args.rec)
    }
}

/// Attach through a registered entry, repairing it over ssh on an early
/// failure: dial the saved route (a kept `direct` first); if nobody answered,
/// start the server over ssh and redial; if still refused, re-pair and redial;
/// if ssh itself failed, report both. `--no-enroll` stops after the first dial.
fn attach_registered(args: &RemoteAttach<'_>, entry: RemoteEntry) -> ExitCode {
    let entry = promote_direct_route(entry);
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
            args.target.ssh_destination()
        } else {
            entry.ssh_destination()
        }
    });
    let quic_port = endpoint_port(&entry.endpoint).unwrap_or(DEFAULT_QUIC_PORT);
    match outcome.repair {
        attach::Repair::None => return outcome.code,
        attach::Repair::Start => {
            eprintln!(
                "phux: {name} is not answering at {}; starting its server over ssh ({ssh_host})...",
                entry.endpoint
            );
            match enroll::ensure_remote_server(
                &ssh_host,
                "phux",
                quic_port,
                enroll::ServicePolicy::Install,
            ) {
                Ok(supervision) => {
                    eprintln!("phux: {name}: {}", supervision.describe());
                    let again =
                        attach::run_attach_remote_outcome(&entry, args.session.clone(), args.rec);
                    if again.repair == attach::Repair::None {
                        return again.code;
                    }
                    eprintln!(
                        "phux: {name} still does not answer with the saved credentials; re-pairing over ssh..."
                    );
                }
                Err(failure) => {
                    eprintln!(
                        "phux: {name}: could not start it over ssh: {}",
                        failure.detail()
                    );
                    eprintln!(
                        "phux: check `ssh {ssh_host}` from this machine, then `phux attach {name}` again; \
                         or, if the server should be up, run `phux doctor` on {name}"
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
        attach::Repair::RePair => {
            eprintln!(
                "phux: {name} refused the saved route ({}); re-pairing over ssh ({ssh_host})...",
                entry.endpoint
            );
        }
    }
    let entry = match register_over_ssh(&args.target, &ssh_host, Some(&entry)) {
        Ok(entry) => entry,
        Err(err) => {
            eprintln!("phux: {name}: {}", err.message);
            eprintln!("phux:   {}", err.remedy);
            return ExitCode::FAILURE;
        }
    };
    attach::run_attach_remote(&entry, args.session.clone(), args.rec)
}

/// An `ssh://` entry that kept a paired `direct` route: dial it briefly and,
/// when it answers, rewrite the entry so this and every later attach go
/// direct. Anything short of an answer leaves the entry as it was.
fn promote_direct_route(entry: RemoteEntry) -> RemoteEntry {
    let Some(direct) = entry.direct.as_deref() else {
        return entry;
    };
    if !entry.endpoint.starts_with("ssh://") {
        return entry;
    }
    let Ok(Endpoint::Quic(target)) = Endpoint::parse(direct) else {
        return entry;
    };
    let Ok(Some(token)) = remote::read_token(&entry) else {
        return entry;
    };
    if enroll::probe(&target, &token, entry.cert_fingerprint.as_deref()).is_err() {
        return entry;
    }
    let promoted = remote::NewRemote::new(
        &entry.name,
        direct,
        entry.token_file.as_deref(),
        entry.cert_fingerprint.as_deref(),
        entry.session.as_deref(),
    )
    .map(|new| new.with_ssh(entry.ssh.as_deref()));
    match promoted.and_then(|new| remote::add_or_update(&new).map(|()| new)) {
        Ok(new) => {
            eprintln!(
                "phux: {}: the direct route answers now; upgraded the registry entry to {direct}",
                entry.name
            );
            RemoteEntry {
                endpoint: new.endpoint,
                direct: None,
                ..entry
            }
        }
        Err(err) => {
            eprintln!(
                "phux: {}: the direct route answers but the entry could not be rewritten ({err}); attaching over ssh",
                entry.name
            );
            entry
        }
    }
}

/// The port of a `quic://` or `wss://` registry endpoint.
fn endpoint_port(endpoint: &str) -> Option<u16> {
    let rest = endpoint.split_once("://").map(|(_, rest)| rest)?;
    let authority = rest.split(['/', '?']).next().unwrap_or(rest);
    let port = authority.rsplit_once(':').map(|(_, port)| port)?;
    port.parse::<u16>().ok().filter(|port| *port != 0)
}

/// Walk the ladder: registered, pasted code, ssh, refuse. Shared with the
/// headless verbs' `--remote` so a target means one host everywhere.
pub(crate) fn resolve(
    target: &RemoteTarget,
    code: Option<&str>,
    bootstrap: Bootstrap,
) -> Result<RemoteEntry, String> {
    // Rung 1: already registered. A pasted `--code` still wins, because the
    // operator is holding fresher credentials than the ones on disk — that
    // is what re-pairing after a revoke looks like.
    if code.is_none()
        && let Some(entry) = find_entry(target)
    {
        return Ok(with_port_override(entry, target));
    }

    // Rung 2: a connect code carries the endpoint, the pin, and the token.
    if let Some(code) = code {
        return register_from_code(target, code);
    }

    // Rung 3: mint credentials over the operator's existing ssh trust.
    if bootstrap == Bootstrap::Never {
        return Err(unregistered_message(target, None));
    }
    register_over_ssh(target, &target.ssh_destination(), None).map_err(|err| {
        unregistered_message(
            target,
            Some(&format!("{}\nphux:   {}", err.message, err.remedy)),
        )
    })
}

/// Register a host from a pasted connect link and return the entry to
/// dial.
fn register_from_code(target: &RemoteTarget, code: &str) -> Result<RemoteEntry, String> {
    let link = pair::parse_connect_link(code).map_err(|err| format!("phux: --code: {err}"))?;

    let name = target.registry_name();
    let token_file = enroll::token_path(&phux_server::telemetry::state_dir(), &name);

    // Validate before the token lands: a rejected name or an unpinned
    // routable endpoint must not leave an orphaned bearer token on disk.
    // Same ordering `phux host add` uses, and for the same reason.
    let new = remote::NewRemote::new(
        &name,
        &link.url,
        Some(&token_file),
        link.cert_fingerprint.as_deref(),
        None,
    )
    .map_err(|err| format!("phux: --code: {err}"))?;

    enroll::write_token(&token_file, &link.token).map_err(|err| format!("phux: --code: {err}"))?;
    remote::add_or_update(&new).map_err(|err| format!("phux: --code: {err}"))?;

    eprintln!(
        "phux: paired {name} -> {} (from the connect code)",
        new.endpoint
    );
    eprintln!("phux: next time, `phux --remote {name}` needs no code");

    Ok(registered_entry(target, &new))
}

/// Read back the entry just registered, so a write that did not round-trip
/// fails here; falls back to the synthesized entry if the config is unreadable.
fn registered_entry(target: &RemoteTarget, new: &remote::NewRemote) -> RemoteEntry {
    find_entry(target).unwrap_or_else(|| RemoteEntry {
        index: 0,
        name: new.name.clone(),
        endpoint: new.endpoint.clone(),
        token_file: new.token_file.clone(),
        cert_fingerprint: new.cert_fingerprint.clone(),
        session: None,
        ssh: None,
        direct: None,
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
    eprintln!("phux: setting {} up over ssh ({ssh_host})...", target.host);

    // The target's own authority when a port was given, so an operator who
    // knows their listener is not on 8788 does not have to enroll
    // separately to say so.
    let endpoint_override = target.port.map(|_| target.authority());
    let previous_token = existing.and_then(|entry| remote::read_token(entry).ok().flatten());
    let previous_fingerprint = existing.and_then(|entry| entry.cert_fingerprint.clone());
    let req = enroll::EnrollRequest {
        ssh_host,
        remote_phux: "phux",
        endpoint_override: endpoint_override.as_deref(),
        quic_port: target.port.unwrap_or(DEFAULT_QUIC_PORT),
        service: enroll::ServicePolicy::Install,
        previous_token: previous_token.as_deref(),
        previous_fingerprint: previous_fingerprint.as_deref(),
    };
    let (entry, outcome) = host::enroll_remote_over_ssh(
        &name,
        &req,
        existing.and_then(|entry| entry.session.as_deref()),
        &mut |event| eprintln!("phux: {name}: {}", event.describe()),
    )?;

    eprintln!("phux: registered {name} -> {}", entry.endpoint);
    if entry.endpoint.starts_with("ssh://") {
        eprintln!(
            "phux: no direct route answered{}; this attach rides ssh, and every attach tries the direct route first",
            if outcome.tried.is_empty() {
                String::new()
            } else {
                format!(" (tried {})", outcome.tried.join(", "))
            }
        );
    } else {
        eprintln!("phux: later attaches dial it directly; ssh is out of the path");
    }
    Ok(entry)
}

/// What to print when a target cannot be resolved: the two remedies, in the
/// order an operator can act on them.
fn unregistered_message(target: &RemoteTarget, ssh_error: Option<&str>) -> String {
    use std::fmt::Write as _;

    let name = target.registry_name();
    let mut message = format!("phux: {name} is not a registered host");
    if let Some(err) = ssh_error {
        // `write!` into a String is infallible; the Result is discarded
        // rather than unwrapped so no error path exists to mishandle.
        let _ = write!(
            message,
            " and setting it up over ssh failed:\nphux:   {err}"
        );
    }
    let _ = write!(message, "\n{}", unregistered_remedies(target));
    message
}

/// The two remedies for an unregistered host, in the order an operator can
/// act on them.
fn unregistered_remedies(target: &RemoteTarget) -> String {
    let name = target.registry_name();
    format!(
        "phux: to pair without ssh, run `phux pair` on {} and paste the link:\
         \nphux:   phux --remote {name} --code '<https://phux.sh/connect?...>'\
         \nphux: or, with ssh access, `phux host add {name}` (starts and supervises the server there)",
        target.host
    )
}

#[cfg(test)]
mod tests {
    use super::{Bootstrap, RemoteTarget, endpoint_host, with_port_override};
    use crate::commands::remote::RemoteEntry;

    fn entry(name: &str, endpoint: &str) -> RemoteEntry {
        RemoteEntry {
            index: 0,
            name: name.to_owned(),
            endpoint: endpoint.to_owned(),
            token_file: None,
            cert_fingerprint: None,
            session: None,
            ssh: None,
            direct: None,
        }
    }

    #[test]
    fn endpoint_port_reads_direct_endpoints() {
        assert_eq!(super::endpoint_port("quic://mini:8788"), Some(8788));
        assert_eq!(super::endpoint_port("wss://[fd7a::1]:8787"), Some(8787));
        assert_eq!(super::endpoint_port("ssh://mini"), None);
    }

    #[test]
    fn parse_labeled_words_errors_for_the_spelling_in_use() {
        let err = RemoteTarget::parse_labeled("", "host add").expect_err("empty");
        assert!(err.starts_with("host add needs a target"), "{err}");
        let err = RemoteTarget::parse_labeled("mini:0", "host add").expect_err("port 0");
        assert!(err.starts_with("host add port"), "{err}");
        assert!(
            RemoteTarget::parse("mini:0")
                .expect_err("port 0")
                .starts_with("--remote port")
        );
    }

    #[test]
    fn parses_every_target_spelling() {
        let bare = RemoteTarget::parse("mini").expect("bare");
        assert_eq!(bare.user, None);
        assert_eq!(bare.host, "mini");
        assert_eq!(bare.port, None);

        let user = RemoteTarget::parse("phall@mini.ts.net").expect("user");
        assert_eq!(user.user.as_deref(), Some("phall"));
        assert_eq!(user.host, "mini.ts.net");

        let port = RemoteTarget::parse("phall@mini:9999").expect("port");
        assert_eq!(port.port, Some(9999));
        assert_eq!(port.host, "mini");
    }

    #[test]
    fn parses_ipv6_with_and_without_a_port() {
        // Bracketed: the port is unambiguous.
        let bracketed = RemoteTarget::parse("me@[fd7a::1]:8788").expect("bracketed");
        assert_eq!(bracketed.host, "fd7a::1");
        assert_eq!(bracketed.port, Some(8788));

        // Bare: `fd7a::1` has no port, and guessing one from the last colon
        // would silently dial a different address.
        let bare = RemoteTarget::parse("fd7a::1").expect("bare v6");
        assert_eq!(bare.host, "fd7a::1");
        assert_eq!(bare.port, None);
        assert_eq!(bare.authority(), "[fd7a::1]:8788");
    }

    #[test]
    fn refuses_targets_that_are_not_host_shaped() {
        // A URI is a real thing operators will try; the error names the verb
        // that does accept one instead of guessing.
        let err = RemoteTarget::parse("quic://mini:8788").expect_err("uri");
        assert!(err.contains("phux host add"), "{err}");

        assert!(RemoteTarget::parse("").is_err());
        assert!(RemoteTarget::parse("  ").is_err());
        assert!(RemoteTarget::parse("@mini").is_err(), "empty user");
        assert!(RemoteTarget::parse("me@").is_err(), "empty host");
        assert!(RemoteTarget::parse("mini:0").is_err(), "port 0");
        assert!(RemoteTarget::parse("mini:70000").is_err(), "port overflow");
        assert!(RemoteTarget::parse("mini:ssh").is_err(), "non-numeric port");
        // Would escape the token directory on join.
        assert!(RemoteTarget::parse("../evil").is_err());
        // Would shadow the selector grammar.
        assert!(RemoteTarget::parse("#tag").is_err());
    }

    #[test]
    fn registry_name_drops_the_port_but_keeps_the_user() {
        // `--remote mini` and `--remote mini:8788` are one machine, so they
        // must not become two registry entries.
        assert_eq!(
            RemoteTarget::parse("mini:8788").expect("t").registry_name(),
            "mini"
        );
        assert_eq!(
            RemoteTarget::parse("phall@mini:8788")
                .expect("t")
                .registry_name(),
            "phall@mini"
        );
    }

    #[test]
    fn authority_defaults_to_the_auto_listen_quic_port() {
        // ADR-0081 binds 8788 without being asked, so a target with no port
        // must mean that one.
        assert_eq!(
            RemoteTarget::parse("mini").expect("t").authority(),
            "mini:8788"
        );
    }

    #[test]
    fn endpoint_host_reads_every_registry_scheme() {
        assert_eq!(endpoint_host("quic://mini:8788").as_deref(), Some("mini"));
        assert_eq!(
            endpoint_host("wss://mini.ts.net:8787").as_deref(),
            Some("mini.ts.net")
        );
        assert_eq!(endpoint_host("ssh://mini").as_deref(), Some("mini"));
        assert_eq!(
            endpoint_host("quic://[fd7a::1]:8788").as_deref(),
            Some("fd7a::1")
        );
        assert_eq!(endpoint_host("not-a-uri"), None);
    }

    #[test]
    fn explicit_port_overrides_the_registered_endpoint() {
        let target = RemoteTarget::parse("mini:9999").expect("t");
        let overridden = with_port_override(entry("mini", "quic://mini:8788"), &target);
        assert_eq!(overridden.endpoint, "quic://mini:9999");

        // The scheme of a ws entry survives the override.
        let ws = with_port_override(entry("mini", "wss://mini:8787"), &target);
        assert_eq!(ws.endpoint, "wss://mini:9999");

        // ssh:// has no port in this grammar: leave it alone rather than
        // fabricating one the operator's ssh config may already answer.
        let ssh = with_port_override(entry("mini", "ssh://mini"), &target);
        assert_eq!(ssh.endpoint, "ssh://mini");
    }

    #[test]
    fn no_explicit_port_leaves_the_entry_untouched() {
        let target = RemoteTarget::parse("mini").expect("t");
        let untouched = with_port_override(entry("mini", "quic://mini:9999"), &target);
        assert_eq!(
            untouched.endpoint, "quic://mini:9999",
            "a registered non-default port must survive a portless target"
        );
    }

    #[test]
    fn bootstrap_never_is_distinct_from_auto() {
        assert_ne!(Bootstrap::Auto, Bootstrap::Never);
    }
}
