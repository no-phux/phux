//! `phux relay` (ADR-0051/ADR-0052): `run` serves the reference relay in the
//! foreground; `pair` enrolls a route and mints or rotates its tunnel token.
//! Both use fixed paths under the state directory.

use std::net::SocketAddr;
use std::process::ExitCode;

use usage::Subcommands;

/// `phux relay <action>` — serve the relay or enroll a route.
#[derive(Debug, Subcommands)]
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
        #[usage(long, value_name = "HOST:PORT")]
        listen: SocketAddr,

        /// Maximum concurrent connections, tunnels and consumers
        /// combined. An over-cap connection is refused after its
        /// handshake completes; existing connections are unaffected.
        #[usage(
            long,
            value_name = "N",
            default = "64", default_value_t = phux_relay::DEFAULT_MAX_CONNS,
            validate = "int(value) >= 1", validate_error = "must be at least 1"
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
        #[usage(long, value_name = "NAME")]
        route: String,
    },
}

/// Dispatch a `phux relay` action.
pub(crate) fn run_relay(action: RelayAction) -> ExitCode {
    // The relay's state is not profile-scoped (`phux_relay` cannot see the
    // profile), so a dev build's relay would otherwise run on, and mint
    // into, the production relay's certificate and route tokens.
    for path in [
        phux_relay::default_relay_cert_path(),
        phux_relay::default_relay_key_path(),
        phux_relay::default_relay_tokens_path(),
    ] {
        if let Err(refusal) = phux_config::production::refuse_dev_on_production_state(&path) {
            eprintln!("phux relay: {refusal}");
            return ExitCode::FAILURE;
        }
    }
    match action {
        RelayAction::Run { listen, max_conns } => run_relay_run(listen, max_conns),
        RelayAction::Pair { route } => run_relay_pair(&route),
    }
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
    // A long-running foreground process arms the durable panic hook itself,
    // like `phux server`.
    phux_server::telemetry::install_server_panic_hook();

    crate::print_banner();

    let mut config = phux_relay::RelayConfig::new(listen);
    config.max_conns = max_conns;

    // Pre-flight the banner's ingredients so a bad token store, certificate,
    // or state dir fails before "listening" is printed.
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

    // Bind before the banner so it shows the resolved address (`:0` becomes the
    // real port).
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

    outln!("Tunnel token for route \"{route}\" (a secret — give it to the phux server once):");
    outln!("  {token}");
    outln!();

    match phux_relay::cert_fingerprint(&cert) {
        Ok(fingerprint) => {
            outln!("Relay certificate SHA-256 (pin it on the dialing side to defeat MITM):");
            outln!("  {fingerprint}");
            outln!();
        }
        Err(err) => {
            eprintln!("phux relay pair: warning: could not read certificate fingerprint: {err}");
        }
    }

    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::banner_line;

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
