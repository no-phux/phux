//! `phux relay` — run the reference relay or enroll a route with it
//! (ADR-0051/ADR-0052).
//!
//! Two verbs front `phux_relay`'s library surface: `run` serves the relay
//! in the foreground (mirroring how `phux server` fronts `ServerRuntime`),
//! and `pair` enrolls a route name into the route-token store, minting —
//! or rotating — the route's tunnel token. Both operate on fixed paths
//! under the phux state directory; neither reads environment overrides.

use std::net::SocketAddr;
use std::process::ExitCode;

use clap::Subcommand;

/// `phux relay <action>` — serve the relay or enroll a route.
#[derive(Debug, Subcommand)]
pub(crate) enum RelayAction {
    /// Run the relay in the foreground.
    ///
    /// Binds one QUIC endpoint on LISTEN and serves both relay legs on
    /// it: phux servers dial out from behind NAT and register a tunnel
    /// for their enrolled route, and remote consumers dial in naming a
    /// route, each spliced onto that route's live tunnel as opaque
    /// bytes. Enroll routes with `phux relay pair`; the token store is
    /// re-read per connection attempt, so pairing a new route (or
    /// revoking one by deleting its line) needs no restart. Serves
    /// until Ctrl-C.
    Run {
        /// Address the relay's QUIC endpoint binds (e.g. `0.0.0.0:4433`).
        /// Always explicit — there is no default listen address, so
        /// exposing the relay requires typing where.
        #[arg(long, value_name = "HOST:PORT")]
        listen: SocketAddr,

        /// Maximum concurrent connections, tunnels and consumers
        /// combined. An over-cap connection is refused after its
        /// handshake completes; existing connections are unaffected.
        #[arg(
            long,
            value_name = "N",
            default_value_t = phux_relay::DEFAULT_MAX_CONNS,
            value_parser = parse_max_conns
        )]
        max_conns: usize,
    },

    /// Enroll a route and mint (or rotate) its tunnel token.
    ///
    /// Writes one entry binding a fresh secret token to NAME in the
    /// relay's route-token store and prints the token once, alongside
    /// the relay certificate's SHA-256 fingerprint. Give both to the
    /// phux server that will dial out to this relay: the token
    /// authenticates its tunnel, and the fingerprint pins the relay's
    /// certificate. Pairing a route that is already enrolled REPLACES
    /// its token (rotation) — exactly one token per route. Revoke a
    /// route by deleting its line from the store. This never contacts a
    /// running relay — it only writes the token file, and a running
    /// relay picks the change up at the next tunnel handshake.
    Pair {
        /// Route name the token is bound to. Consumers select the route
        /// via the TLS server name, so it must be a lowercase DNS
        /// label: `[a-z0-9-]`, at most 63 characters, no leading or
        /// trailing hyphen. Anything else is rejected, never
        /// normalized.
        #[arg(long, value_name = "NAME")]
        route: String,
    },
}

/// Dispatch a `phux relay` action.
pub(crate) fn run_relay(action: RelayAction) -> ExitCode {
    match action {
        RelayAction::Run { listen, max_conns } => run_relay_run(listen, max_conns),
        RelayAction::Pair { route } => run_relay_pair(&route),
    }
}

/// Validate `--max-conns` as a positive connection cap.
fn parse_max_conns(value: &str) -> Result<usize, String> {
    let conns: usize = value
        .parse()
        .map_err(|_| "max-conns must be a whole number".to_owned())?;
    if conns == 0 {
        return Err(
            "max-conns must be at least 1 (a cap of 0 would refuse every connection)".to_owned(),
        );
    }
    Ok(conns)
}

/// The one-line listening banner: address, enrolled route count, and the
/// certificate fingerprint tunnels and consumers will see.
fn banner_line(listen: SocketAddr, routes: usize, fingerprint: &str) -> String {
    format!(
        "phux relay listening on {listen} (routes={routes}; cert sha256 {fingerprint}; \
         Ctrl-C to stop)"
    )
}

/// Foreground `phux relay run`: pre-flight the state files, bind, print
/// the banner (with the resolved listen address), then serve on a
/// current-thread runtime until Ctrl-C — the same wiring shape as
/// `phux server`.
fn run_relay_run(listen: SocketAddr, max_conns: usize) -> ExitCode {
    // Hand-started long-running foreground process, like `phux server`.
    crate::print_banner();

    let mut config = phux_relay::RelayConfig::new(listen);
    config.max_conns = max_conns;

    // Pre-flight the banner's ingredients before the runtime spins up:
    // `run` would fail-fast on the same problems, but only after the
    // human has been told "listening". A malformed token store, broken
    // certificate material, or an unwritable state dir fails here with a
    // clean one-line diagnostic instead.
    let routes = match phux_relay::RouteTokenStore::load(&config.tokens_path) {
        Ok(store) => store.len(),
        Err(err) => {
            eprintln!("phux relay: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = phux_relay::ensure_self_signed(&config.cert_path, &config.key_path) {
        eprintln!("phux relay: could not provision certificate: {err}");
        return ExitCode::FAILURE;
    }
    let fingerprint = match phux_relay::cert_fingerprint(&config.cert_path) {
        Ok(fingerprint) => fingerprint,
        Err(err) => {
            eprintln!("phux relay: could not read certificate fingerprint: {err}");
            return ExitCode::FAILURE;
        }
    };

    // Bind before printing the banner so it can carry the RESOLVED
    // address: `--listen 127.0.0.1:0` shows the OS-assigned port, not the
    // literal 0. The bind needs a runtime context (quinn attaches its I/O
    // driver), so the current-thread runtime is built here — the same
    // shape `RelayRuntime::run` would use.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("phux relay failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(async {
        let bound = phux_relay::RelayRuntime::new(config).bind()?;
        eprintln!("{}", banner_line(bound.local_addr(), routes, &fingerprint));
        bound
            .serve(async {
                // Resolves on SIGINT; either way, the user wants out.
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
    });
    match result {
        Ok(()) => {
            eprintln!("phux relay: shutting down cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux relay failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `phux relay pair --route NAME`: validate the name, provision the relay
/// certificate on first use, mint (or rotate) the route's token, and print
/// the credentials once — the relay-side sibling of `phux pair`.
fn run_relay_pair(route: &str) -> ExitCode {
    // Reject — never normalize — before any file is touched.
    if let Err(err) = phux_relay::validate_route_name(route) {
        eprintln!("phux relay pair: {err}");
        return ExitCode::FAILURE;
    }

    // Provision the self-signed certificate at the fixed paths if it is
    // not there yet, so the fingerprint below is the one the relay will
    // actually present. Best-effort, like `phux pair`: a provisioning
    // problem costs the fingerprint section, not the token.
    let cert = phux_relay::default_relay_cert_path();
    let key = phux_relay::default_relay_key_path();
    if let Err(err) = phux_relay::ensure_self_signed(&cert, &key) {
        eprintln!("phux relay pair: warning: could not provision certificate: {err}");
    }

    let tokens = phux_relay::default_relay_tokens_path();
    let token = match phux_relay::mint_route_token(&tokens, route) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("phux relay pair: failed to mint route token: {err}");
            return ExitCode::FAILURE;
        }
    };

    println!("Tunnel token for route \"{route}\" (a secret — give it to the phux server once):");
    println!("  {token}");
    println!();

    match phux_relay::cert_fingerprint(&cert) {
        Ok(fingerprint) => {
            println!("Relay certificate SHA-256 (pin it on the dialing side to defeat MITM):");
            println!("  {fingerprint}");
            println!();
        }
        Err(err) => {
            eprintln!("phux relay pair: warning: could not read certificate fingerprint: {err}");
        }
    }

    println!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{banner_line, parse_max_conns};

    /// `--max-conns` is validated at parse time: positive integers only,
    /// so a cap of 0 (refuse everything) or garbage never reaches the
    /// runtime.
    #[test]
    fn max_conns_accepts_positive_integers_only() {
        assert_eq!(parse_max_conns("1"), Ok(1));
        assert_eq!(parse_max_conns("64"), Ok(64));
        assert_eq!(parse_max_conns("1024"), Ok(1024));

        assert!(parse_max_conns("0").is_err(), "0 refuses every connection");
        assert!(parse_max_conns("-1").is_err());
        assert!(parse_max_conns("many").is_err());
        assert!(parse_max_conns("6.4").is_err());
        assert!(parse_max_conns("").is_err());
    }

    /// The banner names all three facts an operator needs at a glance:
    /// where the relay listens, how many routes are enrolled, and the
    /// certificate fingerprint the dialing sides will pin.
    #[test]
    fn banner_states_addr_route_count_and_fingerprint() {
        let line = banner_line("127.0.0.1:4433".parse().unwrap(), 2, "AB:CD:EF");
        assert!(line.contains("127.0.0.1:4433"));
        assert!(line.contains("routes=2"));
        assert!(line.contains("AB:CD:EF"));
        assert!(line.contains("Ctrl-C"));
    }
}
