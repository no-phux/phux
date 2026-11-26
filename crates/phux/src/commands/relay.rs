//! `phux relay` (ADR-0051/ADR-0052): `run` serves the reference relay in the
//! foreground; `pair` enrolls a route and mints or rotates its tunnel token;
//! `connect` points this machine's server at a relay with that token. `run`
//! and `pair` use fixed paths under the state directory.
//!
//! `pair --json` emits `{"schema_version":1,"route","token",
//! "cert_fingerprint","tokens_path"}`; `connect --json` emits
//! `{"schema_version":1,"relay","token_file","cert_fingerprint","config_path",
//! "policy_set","tunnel":{"state","error"}}` where `state` is `up`, `down`,
//! or `no_server`. Failures follow [`super::json_err`].

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use usage::Subcommands;

use super::cli::JsonOpt;
use super::json_err::{self, CliError, codes};

/// `phux relay <action>` — serve the relay, enroll a route, or dial one.
#[derive(Debug, Subcommands)]
pub(crate) enum RelayAction {
    /// Run the relay in the foreground
    ///
    /// Binds one QUIC endpoint on LISTEN and serves both relay legs on
    /// it: phux servers dial out from behind NAT and register a tunnel
    /// for their enrolled route, and remote consumers dial in naming a
    /// route, each spliced onto that route's live tunnel as opaque
    /// bytes. Enroll routes with `phux relay pair`; the token store is
    /// re-read whenever it changes, so pairing a new route (or
    /// revoking one by deleting its line) needs no restart. Logs each
    /// tunnel and bridged consumer on stderr. Serves until Ctrl-C.
    Run {
        /// Address the relay's QUIC endpoint binds (e.g. `0.0.0.0:4433`).
        /// Always explicit — there is no default listen address, so
        /// exposing the relay requires typing where.
        #[usage(long, value_name = "HOST:PORT")]
        listen: SocketAddr,

        /// Maximum concurrent connections, tunnels and consumers
        /// combined. An over-cap connection is refused after its
        /// handshake completes; existing connections are unaffected.
        #[usage(
            long,
            value_name = "N",
            default = "64", default_value_t = phux_dial::relay::DEFAULT_MAX_CONNS,
            validate = "int(value) >= 1", validate_error = "must be at least 1"
        )]
        max_conns: usize,
    },

    /// Enroll a route and mint (or rotate) its tunnel token
    ///
    /// Writes one entry binding a fresh secret token to NAME in the
    /// relay's route-token store and prints the token once, with the
    /// relay certificate's SHA-256 fingerprint and the `phux relay
    /// connect` command that hands both to the phux server. Pairing a
    /// route that is already enrolled REPLACES its token (rotation) —
    /// exactly one token per route. Revoke a route by deleting its line
    /// from the store. This never contacts a running relay; a running
    /// relay picks the change up at the next tunnel handshake.
    Pair {
        /// Route name the token is bound to. Consumers select the route
        /// via the TLS server name, so it must be a lowercase DNS
        /// label: `[a-z0-9-]`, at most 63 characters, no leading or
        /// trailing hyphen. Anything else is rejected, never
        /// normalized.
        #[usage(long, value_name = "NAME")]
        route: String,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Have this machine's server dial a relay with a route's token
    ///
    /// Reads the tunnel token `phux relay pair` printed from stdin
    /// (paste it at the prompt, or pipe it), stores it owner-only under
    /// the state directory, and writes the `[[connector]]` entry for
    /// RELAY, replacing one for the same relay. A connector needs
    /// `[policy] mode = "paired"`, so an unset mode is set. A running
    /// server is restarted in place, panes kept, and the command waits
    /// for the tunnel; otherwise the server dials when it next starts.
    /// Then `phux pair --relay-route ROUTE` mints device links.
    Connect {
        /// The relay's `HOST:PORT`, as consumers and this server reach it.
        #[usage(arg, value_name = "HOST:PORT")]
        relay: String,

        /// The relay certificate's SHA-256 fingerprint `phux relay pair`
        /// printed. Required unless RELAY is loopback.
        #[usage(long, value_name = "FP")]
        cert_fingerprint: Option<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },
}

/// Dispatch a `phux relay` action.
pub(crate) fn run_relay(action: RelayAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        RelayAction::Run { listen, max_conns } => {
            guarded(false, || run_relay_run(listen, max_conns))
        }
        RelayAction::Pair { route, json } => {
            guarded(json.json, || run_relay_pair(&route, json.json))
        }
        RelayAction::Connect {
            relay,
            cert_fingerprint,
            json,
        } => connect::run(&relay, cert_fingerprint.as_deref(), json.json, socket),
    }
}

/// Run `body` unless a dev build would touch the relay's state, which is not
/// profile-scoped (`phux_dial::relay` cannot see the profile): a dev
/// build's relay would otherwise serve, and mint into, the production
/// relay's certificate and route tokens.
fn guarded(json: bool, body: impl FnOnce() -> ExitCode) -> ExitCode {
    for path in [
        phux_dial::relay::default_relay_cert_path(),
        phux_dial::relay::default_relay_key_path(),
        phux_dial::relay::default_relay_tokens_path(),
    ] {
        if let Err(refusal) = phux_config::production::refuse_dev_on_production_state(&path) {
            let err = CliError::new(
                codes::USAGE_ERROR,
                format!("relay: {refusal}"),
                // The refusal carries its own remedies as hint lines.
                String::new(),
            );
            return json_err::emit(json, &err, 1);
        }
    }
    body()
}

/// The listening banner: where the relay listens, then the enrolled routes
/// and the certificate fingerprint tunnels and consumers will pin.
fn banner(listen: SocketAddr, routes: &[String], fingerprint: &str) -> String {
    let routes = if routes.is_empty() {
        "none yet (enroll one with `phux relay pair --route NAME`)".to_owned()
    } else {
        routes.join(", ")
    };
    format!(
        "phux relay listening on {listen}\n  routes  {routes}\n  cert    sha256 \
         {fingerprint}\n  Ctrl-C to stop"
    )
}

/// Foreground `phux relay run`: pre-flight the state files, bind, print
/// the banner (with the resolved listen address), then serve on a
/// current-thread runtime until Ctrl-C — the same wiring shape as
/// `phux server`.
fn run_relay_run(listen: SocketAddr, max_conns: usize) -> ExitCode {
    // A long-running foreground process arms the durable panic hook itself,
    // like `phux server`.
    phux_server::telemetry::install_server_panic_hook();

    crate::print_banner();

    let mut config = phux_dial::relay::config(listen);
    config.max_conns = max_conns;

    // Pre-flight the banner's ingredients so a bad token store, certificate,
    // or state dir fails before "listening" is printed.
    let routes: Vec<String> = match phux_dial::relay::RouteTokenStore::load(&config.tokens_path) {
        Ok(store) => store.routes().into_iter().collect(),
        Err(err) => {
            error!("relay: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = phux_dial::relay::ensure_self_signed(&config.cert_path, &config.key_path) {
        error!("relay: could not provision certificate: {err}");
        return ExitCode::FAILURE;
    }
    let fingerprint = match phux_dial::relay::cert_fingerprint(&config.cert_path) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            error!("relay: could not read certificate fingerprint: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Bind before the banner so it shows the resolved address (`:0` becomes the
    // real port).
    let rt = match crate::commands::build_cli_runtime() {
        Ok(rt) => rt,
        Err(err) => {
            error!("relay failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(async {
        let bound = phux_dial::relay::RelayRuntime::new(config).bind()?;
        // Listener banner is the foreground runtime contract.
        eprintln!("{}", banner(bound.local_addr(), &routes, &fingerprint));
        bound
            .serve(async {
                // Resolves on SIGINT; either way, the user wants out.
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
    });
    match result {
        Ok(()) => {
            info!("relay: shutting down cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!("relay failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// What `phux relay pair` minted.
#[derive(Debug, serde::Serialize)]
struct Enrolled {
    schema_version: u32,
    route: String,
    token: String,
    /// `None` when the certificate could not be provisioned or read.
    cert_fingerprint: Option<String>,
    tokens_path: PathBuf,
}

/// `phux relay pair --route NAME`: validate the name, provision the relay
/// certificate on first use, mint (or rotate) the route's token, and print
/// the credentials once — the relay-side sibling of `phux pair`.
fn run_relay_pair(route: &str, json: bool) -> ExitCode {
    match enroll_route(route) {
        Ok(enrolled) if json => crate::output::json(&enrolled),
        Ok(enrolled) => {
            print_enrolled(&enrolled);
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(json, &err, 1),
    }
}

fn enroll_route(route: &str) -> Result<Enrolled, CliError> {
    // Reject — never normalize — before any file is touched.
    phux_dial::relay::validate_route_name(route).map_err(|err| {
        CliError::new(
            codes::USAGE_ERROR,
            format!("relay pair: {err}"),
            "name the route with a lowercase DNS label such as `home` or `lab-1`",
        )
    })?;

    // Provision the self-signed certificate at the fixed paths if it is not
    // there yet, so the fingerprint is the one the relay will present.
    // Best-effort: a provisioning problem costs the fingerprint, not the token.
    let cert = phux_dial::relay::default_relay_cert_path();
    let key = phux_dial::relay::default_relay_key_path();
    if let Err(err) = phux_dial::relay::ensure_self_signed(&cert, &key) {
        warn!("relay pair: could not provision certificate: {err}");
    }

    let tokens_path = phux_dial::relay::default_relay_tokens_path();
    let token = phux_dial::relay::mint_route_token(&tokens_path, route).map_err(|err| {
        CliError::new(
            codes::IO,
            format!("relay pair: could not mint the route token: {err}"),
            format!("check that {} is writable", tokens_path.display()),
        )
    })?;
    let cert_fingerprint = phux_dial::relay::cert_fingerprint(&cert)
        .inspect_err(|err| warn!("relay pair: could not read certificate fingerprint: {err}"))
        .ok();
    Ok(Enrolled {
        schema_version: 1,
        route: route.to_owned(),
        token,
        cert_fingerprint,
        tokens_path,
    })
}

fn print_enrolled(enrolled: &Enrolled) {
    let route = &enrolled.route;
    outln!("Tunnel token for route \"{route}\" (a secret, shown once):");
    outln!("  {}", enrolled.token);
    outln!();
    let pin = enrolled
        .cert_fingerprint
        .as_deref()
        .map_or_else(String::new, |fingerprint| {
            outln!("Relay certificate SHA-256:");
            outln!("  {fingerprint}");
            outln!();
            format!(" --cert-fingerprint {fingerprint}")
        });
    outln!("On the phux server that will dial this relay, run this and paste the token:");
    outln!("  phux relay connect RELAY_HOST:PORT{pin}");
    outln!("Then mint each device's link there:");
    outln!("  phux pair --relay-route {route}");
    outln!();
    outln!(
        "Pairing \"{route}\" again replaces its token. Route tokens: {}",
        enrolled.tokens_path.display()
    );
}

/// Run `phux relay` as parsed.
pub(crate) fn run(args: crate::commands::RelayArgs, socket: Option<PathBuf>) -> ExitCode {
    run_relay(args.action, socket)
}

mod connect;

#[cfg(test)]
mod tests {
    use super::banner;

    /// The banner leads with the listen address (scripts read the first
    /// line), then names the enrolled routes and the fingerprint.
    #[test]
    fn banner_states_addr_routes_and_fingerprint() {
        let text = banner(
            "127.0.0.1:4433".parse().unwrap(),
            &["home".to_owned(), "lab".to_owned()],
            "AB:CD:EF",
        );
        let first = text.lines().next().unwrap();
        assert_eq!(first, "phux relay listening on 127.0.0.1:4433");
        assert!(text.contains("routes  home, lab"));
        assert!(text.contains("sha256 AB:CD:EF"));
        assert!(text.contains("Ctrl-C"));
        let empty = banner("127.0.0.1:4433".parse().unwrap(), &[], "AB");
        assert!(empty.contains("phux relay pair --route NAME"), "{empty}");
    }
}
