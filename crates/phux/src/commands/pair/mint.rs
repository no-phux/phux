//! The `phux pair` entry point and the mint it runs: settle the flags,
//! check the door is open, then mint and present the credential.

use std::path::PathBuf;
use std::process::ExitCode;

use super::{
    Door, ENROLL_NEEDS_AUTHORITY, ENROLL_NEEDS_QUIC, Lifetime, LinkOutput, PairAction, RelayRoute,
    bound_remote_listener, link_refusal, mint_pairing_bundle, mint_relay_link, parse_expires,
    phone_enrollment_requested, prepare_secret_file, present_listener_credential,
    provision_pairing_certificate, resolve_certificate_paths, resolve_pair_addresses,
    run_credential_action, shows_default_qr,
};

/// Mint a token into the store and print it with the certificate
/// fingerprint. Defaults are the shared paths the server reads (ADR-0031), and
/// the certificate is provisioned if absent. Nothing is minted unless the
/// server on `socket` has a remote listener bound (ADR-0141). With a
/// reachable wss listener the credentials are also printed as a connect
/// link, followed on a terminal by its QR unless `--no-qr`; `--qr` is the
/// camera form, the QR alone. With `relay` the credential is minted for a
/// relay route instead ([`mint_relay_link`]).
pub(crate) fn run(args: crate::commands::PairArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::PairArgs {
        action,
        tokens,
        cert,
        qr,
        secret_file,
        expires,
        no_qr,
        host,
        name,
        json,
        replace_token,
        relay_route,
        relay,
        enroll,
        no_enroll,
        reusable,
        enable,
    } = args;
    run_pair(
        action,
        socket,
        tokens,
        cert,
        qr,
        secret_file,
        expires,
        no_qr,
        host,
        name,
        json,
        replace_token,
        relay_route.map_or(Door::Listener { enroll }, |route| Door::Relay {
            route: RelayRoute { route, relay },
            enroll,
        }),
        enable,
        Lifetime {
            reusable,
            no_enroll,
        },
    )
}

/// The mint behind [`run`], with the parsed flags settled into a door and
/// a lifetime.
#[allow(
    clippy::needless_pass_by_value,
    reason = "CLI entry point owns the args clap dispatch hands it; taking them by value keeps the call site clean"
)]
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "the existing mint options remain flat for CLI compatibility while the optional action selects lifecycle operations"
)]
fn run_pair(
    action: Option<PairAction>,
    socket: Option<PathBuf>,
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    qr: bool,
    secret_file: Option<PathBuf>,
    expires: Option<String>,
    no_qr: bool,
    host: Option<String>,
    name: Option<String>,
    json: bool,
    replace_token: Option<String>,
    door: Door,
    enable: bool,
    lifetime: Lifetime,
) -> ExitCode {
    let tokens = tokens
        .or_else(|| std::env::var_os("PHUX_WS_TOKENS").map(PathBuf::from))
        .unwrap_or_else(phux_server::auth::default_token_store_path);
    if let Some(action) = action {
        return run_credential_action(&tokens, action, json);
    }
    if json && (secret_file.is_some() || qr) {
        error!("pair: --json prints the bearer and cannot combine with --qr or --secret-file");
        return ExitCode::from(2);
    }
    if let Some(path) = secret_file.as_deref()
        && let Err(err) = prepare_secret_file(path)
    {
        error!("pair: {err}");
        return ExitCode::FAILURE;
    }
    let expires_for = match expires.as_deref().map(parse_expires).transpose() {
        Ok(duration) => duration,
        Err(err) => {
            error!("pair: {err}");
            return ExitCode::from(2);
        }
    };
    let certificate = resolve_certificate_paths(cert);

    // Everything that could make the credential useless is settled before
    // the store is touched. The store's own refusal comes first, in the
    // words a failed mint uses (a retired legacy store names its remedy).
    if let Err(err) = phux_server::auth::TokenStore::load(&tokens) {
        error!("pair: failed to mint token: {err}");
        return ExitCode::FAILURE;
    }
    let socket = socket.unwrap_or_else(phux_config::socket::default_socket_path);
    let expires_at = expires_for.map(|duration| chrono::Utc::now() + duration);
    let first_use_by = lifetime.first_use_by(expires_at.is_some(), json, &door);
    let output = LinkOutput {
        name: name.as_deref(),
        qr,
        default_qr: shows_default_qr(
            qr || no_qr || json || secret_file.is_some(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        ),
        json,
        secret_file: secret_file.as_deref(),
        expires_at,
        expires_label: expires.as_deref(),
        first_use_by,
        no_enroll: lifetime.no_enroll,
    };
    let enroll = match door {
        Door::Listener { enroll } => enroll,
        Door::Relay { route, enroll } => {
            let replace = replace_token.as_deref();
            let enroll = phone_enrollment_requested(enroll, output, true);
            return mint_relay_link(&route, &socket, &tokens, output, replace, enroll);
        }
    };
    let Some(live) = bound_remote_listener(&socket, enable, json) else {
        return ExitCode::FAILURE;
    };
    let addresses = resolve_pair_addresses(host.as_deref(), live);
    // Both explicit camera QR and a QR shown by default must enroll a new
    // phone. With no link there is no default QR; ordinary mints still work.
    let enroll = phone_enrollment_requested(enroll, output, addresses.server_url.is_some());
    let refusal = link_refusal(
        host.as_deref(),
        qr,
        &addresses.live,
        addresses.server_url.as_deref(),
    )
    .or_else(|| {
        let camera = qr || (output.default_qr && addresses.server_url.is_some());
        (enroll && !addresses.enrollment_endpoint_ready(camera))
            .then(|| ENROLL_NEEDS_QUIC.to_owned())
    });
    if let Some(refusal) = refusal {
        error!("pair: {refusal}");
        return ExitCode::FAILURE;
    }
    provision_pairing_certificate(&certificate, &addresses.advertised);
    // A device enrolls trusting the CA its link pins and never learns one
    // (ADR-0179), so a certificate no workload CA issued has nothing to give
    // it. Refused before anything is minted.
    if enroll
        && phux_server::transport::tls::presented_authority(&certificate.cert)
            .ok()
            .flatten()
            .is_none()
    {
        error!("pair: {ENROLL_NEEDS_AUTHORITY}");
        return ExitCode::FAILURE;
    }

    let Some((minted, ticket)) =
        mint_pairing_bundle(&tokens, replace_token.as_deref(), output, enroll)
    else {
        return ExitCode::FAILURE;
    };
    present_listener_credential(
        &minted,
        ticket.as_ref(),
        &tokens,
        &certificate,
        &addresses,
        output,
    )
}
