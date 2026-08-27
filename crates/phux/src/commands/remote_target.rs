//! `phux --remote [USER@]HOST[:PORT]` — the one-shot remote front door.
//!
//! Everything a remote attach needs already existed: a QUIC transport
//! (ADR-0007), auto-provisioned TLS with a token store (ADR-0031), a
//! `[[remote]]` registry that turns a name into an endpoint + pin + token
//! (ADR-0055), a listener the server binds on its overlay address without
//! being asked (ADR-0081). What did not exist is the spelling people
//! actually reach for. `ssh user@host` needs no prior setup on the client,
//! so `phux --remote user@host` must not either.
//!
//! This module is the resolution ladder that closes that gap, cheapest rung
//! first:
//!
//! 1. **A registered host.** The steady state, and the whole point: a
//!    `[[remote]]` entry supplies the endpoint, the pin, and the token, so
//!    the dial is a direct QUIC connection with no ssh anywhere in it.
//! 2. **A pasted connect code.** `--code 'phux://connect?...'` — the same
//!    artifact `phux pair --qr` renders for a phone. Registers the host from
//!    the link and dials. No ssh, no shell on the far end.
//! 3. **A one-time ssh bootstrap.** No entry and no code: run `phux pair`
//!    on the far end over the operator's existing ssh trust, register what
//!    it mints, and dial. This happens once per host; rung 1 catches every
//!    later invocation.
//! 4. **An honest refusal** naming both remedies, when ssh cannot help.
//!
//! ## What `user@` means here
//!
//! It is a *label*, not a wire identity. phux runs one server per user
//! (ADR-0003) and the QUIC preamble carries a bearer token, not a username —
//! so which server you reach is decided by the address and port, and the
//! `user@` half only names the ssh destination for rung 3 and the registry
//! key that remembers the result. Two users on one host are two ports (or
//! two registry entries), not one endpoint disambiguated on the wire. Being
//! explicit about this is what keeps `--remote` from reading as a promise
//! the protocol does not make.
//!
//! ## Why the ssh rung does not install a service
//!
//! `phux host enroll` installs a launchd/systemd unit on the far end,
//! because the operator typed a verb whose whole subject is that host.
//! `phux --remote` is an attach; silently installing a supervised service on
//! someone's machine is a side effect well past what the request implies. So
//! rung 3 runs `phux pair --json` and nothing else — a credential the
//! operator could have minted by hand over the same ssh — and points at
//! `phux host enroll` for the always-on setup.

use std::process::ExitCode;

use super::attach;
use super::enroll;
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
    /// Parse `host`, `user@host`, `host:port`, `user@host:port`, and their
    /// bracketed-IPv6 spellings.
    ///
    /// Rejects a URI rather than guessing at it: `--remote quic://mini:8788`
    /// is a real thing an operator will try, and the endpoint they want is
    /// already expressible through `phux host add`, so the error names that
    /// instead of quietly accepting a second endpoint grammar.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("--remote needs a target, e.g. --remote me@mini".to_owned());
        }
        if trimmed.contains("://") {
            return Err(format!(
                "--remote takes [USER@]HOST[:PORT], not a URI (got {trimmed:?}); \
                 register a full endpoint with `phux host add NAME {trimmed}`"
            ));
        }

        // Split on the LAST `@`: a username cannot contain `@`, but this way
        // an address that does is still parsed the way the operator meant.
        let (user, rest) = match trimmed.rsplit_once('@') {
            Some((user, rest)) => {
                if user.is_empty() {
                    return Err(format!("--remote target {trimmed:?} has an empty user"));
                }
                (Some(user.to_owned()), rest)
            }
            None => (None, trimmed),
        };

        let (host, port) = split_host_port(rest)?;
        if host.is_empty() {
            return Err(format!("--remote target {trimmed:?} has an empty host"));
        }
        // A registry name may not contain `/` (it would escape the token
        // directory on join) or a selector sigil. Catch it here, where the
        // message can point at what the operator typed.
        if host.contains('/') || host.starts_with(['@', '#', '.', '=']) {
            return Err(format!(
                "--remote host {host:?} must not contain '/' or start with a selector sigil (@ # . =)"
            ));
        }

        Ok(Self { user, host, port })
    }

    /// The registry key this target remembers itself under: the spelling the
    /// operator typed, minus any port.
    ///
    /// Port is excluded on purpose. It belongs to the endpoint, and folding
    /// it into the name would make `--remote mini` and `--remote mini:8788`
    /// two hosts that are one machine.
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
fn split_host_port(rest: &str) -> Result<(String, Option<u16>), String> {
    if let Some(inner) = rest.strip_prefix('[') {
        let (host, tail) = inner
            .split_once(']')
            .ok_or_else(|| format!("--remote target {rest:?} has an unclosed '['"))?;
        let port = match tail {
            "" => None,
            tail => Some(parse_port(tail.strip_prefix(':').ok_or_else(|| {
                format!("--remote target {rest:?} has trailing text after ']'")
            })?)?),
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
        Some((host, port)) => Ok((host.to_owned(), Some(parse_port(port)?))),
        None => Ok((rest.to_owned(), None)),
    }
}

fn parse_port(raw: &str) -> Result<u16, String> {
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| format!("--remote port {raw:?} must be 1..=65535"))
}

/// Find the registry entry that already describes this target.
///
/// Three matches, widest last, so an operator who enrolled `mini` with
/// `phux host enroll` still gets a hit from `phux --remote me@mini`:
///
/// 1. the exact `user@host` spelling;
/// 2. the bare host (what `enroll::default_name` registers);
/// 3. any entry whose endpoint points at this host.
///
/// A config that cannot be read yields `None` rather than an error: the
/// bootstrap rungs below are a working answer for an unregistered host, and
/// that is exactly what an unreadable registry looks like from here.
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

/// Apply an explicit `:PORT` from the target to a registered entry.
///
/// An override, not a rewrite: the config file is untouched, because
/// `--remote mini:9999` is a statement about this dial, not a correction to
/// what `mini` means. `ssh://` entries have no port to override.
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
    /// A pasted `phux://connect?...` code, for the ssh-free cold path.
    pub(crate) code: Option<&'a str>,
    /// Whether a cold target may bootstrap over ssh.
    pub(crate) bootstrap: Bootstrap,
    /// Recording spec for the attach that follows.
    pub(crate) rec: Option<&'a RecordSpec>,
}

/// Resolve a `--remote` target to a registered host and attach to it.
pub(crate) fn run(args: RemoteAttach<'_>) -> ExitCode {
    let entry = match resolve(&args) {
        Ok(entry) => entry,
        Err(err) => {
            for line in err.lines() {
                eprintln!("{line}");
            }
            return ExitCode::FAILURE;
        }
    };
    attach::run_attach_remote(&entry, args.session, args.rec)
}

/// Walk the ladder: registered, then pasted code, then ssh, then refuse.
fn resolve(args: &RemoteAttach<'_>) -> Result<RemoteEntry, String> {
    let target = &args.target;

    // Rung 1: already registered. A pasted `--code` still wins, because the
    // operator is holding fresher credentials than the ones on disk — that
    // is what re-pairing after a revoke looks like.
    if args.code.is_none()
        && let Some(entry) = find_entry(target)
    {
        return Ok(with_port_override(entry, target));
    }

    // Rung 2: a connect code carries the endpoint, the pin, and the token.
    if let Some(code) = args.code {
        return register_from_code(target, code);
    }

    // Rung 3: mint credentials over the operator's existing ssh trust.
    if args.bootstrap == Bootstrap::Never {
        return Err(unregistered_message(target, None));
    }
    register_over_ssh(target)
}

/// Register a host from a pasted `phux://connect?...` link and return the
/// entry to dial.
fn register_from_code(target: &RemoteTarget, code: &str) -> Result<RemoteEntry, String> {
    let link = pair::parse_connect_link(code).map_err(|err| format!("phux: --code: {err}"))?;

    let name = target.registry_name();
    let token_file = enroll::token_path(&phux_server::telemetry::state_dir(), &name);

    // Validate before the token lands: a rejected name or an unpinned
    // routable endpoint must not leave an orphaned bearer token on disk.
    // Same ordering `phux host enroll` uses, and for the same reason.
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

/// Read back the entry that was just registered.
///
/// Re-reading rather than synthesizing gives the returned entry its real
/// registry index and turns a write that did not round-trip into a failure
/// here, next to the write, instead of a confusing dial later. The synthesized
/// fallback keeps a readable-but-unparseable config from blocking an attach
/// whose credentials are already on disk.
fn registered_entry(target: &RemoteTarget, new: &remote::NewRemote) -> RemoteEntry {
    find_entry(target).unwrap_or_else(|| RemoteEntry {
        index: 0,
        name: new.name.clone(),
        endpoint: new.endpoint.clone(),
        token_file: new.token_file.clone(),
        cert_fingerprint: new.cert_fingerprint.clone(),
        session: None,
    })
}

/// Mint credentials on the far end over ssh, register them, and return the
/// entry to dial.
///
/// Deliberately does NOT install a service on the remote — see this module's
/// header. The one command it runs there is `phux pair --json`.
fn register_over_ssh(target: &RemoteTarget) -> Result<RemoteEntry, String> {
    let destination = target.ssh_destination();
    eprintln!(
        "phux: {} is not registered — pairing over ssh…",
        target.host
    );

    let outcome = enroll::enroll_over_ssh(
        &destination,
        // The target's own authority when a port was given, so an operator
        // who knows their listener is not on 8788 does not have to enroll
        // separately to say so.
        target.port.map(|_| target.authority()).as_deref(),
        DEFAULT_QUIC_PORT,
        // No service install: an attach must not leave a supervised unit
        // behind on someone else's machine.
        false,
        &mut |_| {},
    )
    .map_err(|failure| ssh_failure_message(target, &failure))?;

    let name = target.registry_name();
    let token_file = (!outcome.endpoint.starts_with("ssh://"))
        .then(|| enroll::token_path(&phux_server::telemetry::state_dir(), &name));

    let new = remote::NewRemote::new(
        &name,
        &outcome.endpoint,
        token_file.as_deref(),
        outcome.report.cert_fingerprint.as_deref(),
        None,
    )
    .map_err(|err| format!("phux: pairing {destination}: {err}"))?;

    if let Some(path) = token_file.as_deref() {
        enroll::write_token(path, &outcome.report.token)
            .map_err(|err| format!("phux: pairing {destination}: {err}"))?;
    }
    remote::add_or_update(&new).map_err(|err| format!("phux: pairing {destination}: {err}"))?;

    eprintln!("phux: paired {name} -> {}", new.endpoint);
    if new.endpoint.starts_with("ssh://") {
        eprintln!(
            "phux: {} advertised no dialable listener, so this attach rides ssh.",
            target.host
        );
        eprintln!(
            "phux:   `phux host enroll {destination}` installs a service there and \
             gives you a direct QUIC dial."
        );
    } else {
        eprintln!("phux: later attaches dial it directly — ssh is out of the path");
    }

    Ok(registered_entry(target, &new))
}

/// The report for an ssh bootstrap that could not run.
fn ssh_failure_message(target: &RemoteTarget, failure: &enroll::EnrollFailure) -> String {
    let detail = match failure {
        enroll::EnrollFailure::MissingPhux(err) | enroll::EnrollFailure::Pair(err) => err,
    };
    unregistered_message(target, Some(detail))
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
        let _ = write!(message, " and pairing over ssh failed:\nphux:   {err}");
    }
    let _ = write!(
        message,
        "\nphux: to pair without ssh, run `phux pair` on {} and paste the link:\
         \nphux:   phux --remote {name} --code '<phux://connect?...>'\
         \nphux: or, with ssh access, `phux host enroll {name}` (also installs a service there)",
        target.host
    );
    message
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
        }
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
