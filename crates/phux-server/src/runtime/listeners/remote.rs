//! Bind remote transports with the executing host's TLS and token policy.

use crate::runtime::{ServerEnv, workload_auth};
use phux_protocol::wire::{ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, warn};

/// A disabled listener slot for `GET_STATE`, paired with no listener.
pub(in crate::runtime) const fn disabled<L>(
    transport: RemoteListenerTransport,
    addr: String,
    reason: ListenerDisabledReason,
) -> (Option<L>, RemoteListenerSlot) {
    (
        None,
        RemoteListenerSlot::disabled(transport, Some(addr), reason),
    )
}

/// `PHUX_WS_SECURE=1` forces the secure path on a loopback address, for
/// testing the remote path locally.
pub(in crate::runtime) const fn secure_bind(addr: SocketAddr, env: &ServerEnv) -> bool {
    !addr.ip().is_loopback() || env.ws_secure
}

/// Build the optional WebSocket listener (ADR-0031). The bind address is the
/// toggle: loopback is plaintext and unauthenticated; a routable address (or
/// workload mTLS, which must not have a plaintext side door) gets TLS with an
/// auto-provisioned certificate plus bearer-token auth. Setup failure
/// disables only this listener; the slot records the outcome for
/// `GET_STATE`.
pub(in crate::runtime) async fn build_ws_listener(
    addr: SocketAddr,
    workload_mtls: bool,
    env: &ServerEnv,
) -> (Option<crate::transport::WsListener>, RemoteListenerSlot) {
    const WSS: RemoteListenerTransport = RemoteListenerTransport::Wss;
    let addr_s = addr.to_string();
    let workload = match workload_auth::WorkloadAuth::for_posture(workload_mtls, env) {
        Ok(workload) => workload,
        Err(err) => {
            error!(error = %err, "configured workload mTLS unavailable; WebSocket disabled");
            return disabled(WSS, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };

    if !secure_bind(addr, env) && workload.is_none() {
        let origins = crate::transport::AllowedOrigins::parse(env.ws_allowed_origins.as_deref());
        return match crate::transport::WsListener::bind(addr, origins).await {
            Ok(ws) => {
                let bound = ws.local_addr().map_or(addr_s, |a| a.to_string());
                info!(addr = %bound, "WebSocket listening (plaintext, loopback)");
                (Some(ws), RemoteListenerSlot::bound(WSS, bound))
            }
            Err(err) => {
                warn!(addr = %addr, error = %err, "failed to bind WebSocket; UDS only");
                disabled(WSS, addr_s, ListenerDisabledReason::BindFailed)
            }
        };
    }

    let Some((cert_path, key_path)) = remote_certificate(addr, "wss", env) else {
        return disabled(WSS, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let acceptor = match crate::transport::tls::acceptor_from_pem_with_client_ca(
        &cert_path,
        &key_path,
        workload.as_ref().map(|auth| &auth.ca),
    ) {
        Ok(acceptor) => acceptor,
        Err(err) => {
            error!(error = %err, "TLS setup failed; WebSocket disabled");
            return disabled(WSS, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };
    let store = remote_token_store(&env.tokens_path(), "wss");
    let token_count = store.len();
    let workload_mtls = workload.is_some();
    match crate::transport::WsListener::bind_secure(
        addr,
        acceptor,
        std::sync::Arc::new(store),
        workload.map(|auth| auth.registry),
    )
    .await
    {
        Ok(ws) => {
            let bound = ws.local_addr().map_or(addr_s, |a| a.to_string());
            info!(addr = %bound, tokens = token_count, workload_mtls, "WebSocket listening with TLS + token auth");
            (Some(ws), RemoteListenerSlot::bound(WSS, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind secure WebSocket; UDS only");
            disabled(WSS, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// The credential store a secure remote listener gates admission on. Loaded
/// leniently: a store that will not load at boot admits nobody but reloads on
/// the next authentication, so it never takes the remote surface down until
/// a restart.
pub(in crate::runtime) fn remote_token_store(
    tokens_path: &Path,
    transport: &str,
) -> crate::auth::ReloadingTokenStore {
    let (store, error) = crate::auth::ReloadingTokenStore::load_deferred(tokens_path.to_path_buf());
    if let Some(error) = error {
        warn!(
            error = %error,
            path = %tokens_path.display(),
            "credential store does not load; {transport} listens anyway and refuses every \
             authentication until it does -- run `phux doctor` for the remedy"
        );
    } else if store.is_empty() {
        warn!(
            path = %tokens_path.display(),
            "no pairing tokens; run `phux pair` -- it takes effect immediately, with no restart"
        );
    }
    store
}

/// Log, do not fail, when the certificate does not name the bound address
/// (ADR-0091). phux consumers pin the fingerprint and ignore the name, and
/// widening the SANs would change the fingerprint and un-pair every device;
/// `phux doctor` carries the remedy.
pub(in crate::runtime) fn warn_if_cert_omits_bind(
    cert_path: &Path,
    advertised: &[String],
    transport: &str,
) {
    let uncovered = match crate::transport::tls::uncovered_names(cert_path, advertised) {
        Ok(uncovered) => uncovered,
        Err(err) => {
            // The acceptor build reports an unreadable certificate itself.
            debug!(error = %err, "could not check certificate name coverage");
            return;
        }
    };
    if !uncovered.is_empty() {
        warn!(
            transport,
            addresses = %uncovered.join(", "),
            cert = %cert_path.display(),
            "certificate does not name this listener's address; fingerprint-pinning \
             consumers are unaffected, but a client that validates the server name \
             will refuse the handshake -- run `phux doctor` for the remedy"
        );
    }
}

/// Certificate and key a TLS listener bound to `addr` presents: the
/// operator's (`PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY`) when set, otherwise
/// the shared pair, issued by the workload CA on first use (ADR-0153) and
/// never regenerated (ADR-0091), so every listener presents the one
/// fingerprint `phux pair` prints. `None`, after logging, when it cannot be provisioned.
pub(in crate::runtime) fn remote_certificate(
    addr: SocketAddr,
    transport: &str,
    env: &ServerEnv,
) -> Option<(PathBuf, PathBuf)> {
    let operator_cert = env.tls_cert.is_some() || env.tls_key.is_some();
    let cert_path = env
        .tls_cert
        .clone()
        .unwrap_or_else(crate::transport::tls::default_cert_path);
    let key_path = env
        .tls_key
        .clone()
        .unwrap_or_else(crate::transport::tls::default_key_path);
    if let Err(refusal) = refuse_dev_on_production_credentials(&cert_path, &key_path, env) {
        error!(transport, "{refusal}; listener disabled");
        return None;
    }
    let advertised = crate::transport::tls::advertised_for_bind(addr);
    if !operator_cert
        && let Err(err) = crate::transport::tls::ensure_server_identity(
            &cert_path,
            &key_path,
            &advertised,
            &env.workload_paths(),
        )
    {
        error!(error = %err, transport, "failed to provision the server certificate; listener disabled");
        return None;
    }
    warn_if_cert_omits_bind(&cert_path, &advertised, transport);
    Some((cert_path, key_path))
}

/// A development build never presents the production certificate or admits
/// against the production credential store, which an inherited
/// `PHUX_WS_TLS_*` / `PHUX_WS_TOKENS` (a production pane exports them) would
/// otherwise hand it. Every remote listener resolves its certificate through
/// [`remote_certificate`], so this gates them all.
pub(in crate::runtime) fn refuse_dev_on_production_credentials(
    cert_path: &Path,
    key_path: &Path,
    env: &ServerEnv,
) -> Result<(), String> {
    crate::transport::tls::refuse_dev_on_production_tls(cert_path, key_path)
        .map_err(|err| err.to_string())?;
    phux_config::production::refuse_dev_on_production_state(&env.tokens_path())
}

/// Bearer-token store for a QUIC-class listener: required on a secure bind,
/// absent on loopback.
pub(in crate::runtime) fn remote_tokens(
    secure: bool,
    transport: &str,
    env: &ServerEnv,
) -> Option<std::sync::Arc<crate::auth::ReloadingTokenStore>> {
    secure.then(|| std::sync::Arc::new(remote_token_store(&env.tokens_path(), transport)))
}

/// Build the optional QUIC listener for `addr` (ADR-0007). QUIC is always
/// TLS 1.3 and shares the certificate and token store with `wss://`; a
/// routable address (or `PHUX_WS_SECURE=1`) also requires a bearer-token
/// preamble (ADR-0031). Setup failure disables only this listener.
pub(in crate::runtime) fn build_quic_listener_for(
    addr: SocketAddr,
    workload_mtls: bool,
    env: &ServerEnv,
) -> (
    Option<crate::transport::quic::QuicListener>,
    RemoteListenerSlot,
) {
    const QUIC: RemoteListenerTransport = RemoteListenerTransport::Quic;
    let secure = secure_bind(addr, env);
    let addr_s = addr.to_string();
    let Some((cert_path, key_path)) = remote_certificate(addr, "quic", env) else {
        return disabled(QUIC, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let tokens = remote_tokens(secure, "quic", env);
    let token_count = tokens.as_ref().map_or(0, |s| s.len());
    let workload_auth = match workload_auth::WorkloadAuth::for_posture(workload_mtls, env) {
        Ok(auth) => auth,
        Err(err) => {
            error!(error = %err, "configured workload mTLS unavailable; QUIC disabled");
            return disabled(QUIC, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };
    let (workload_ca, workload_registry) =
        workload_auth.map_or((None, None), |auth| (Some(auth.ca), Some(auth.registry)));
    match crate::transport::quic::QuicListener::from_pem_with_client_ca_and_registry(
        addr,
        &cert_path,
        &key_path,
        tokens,
        workload_ca.as_ref(),
        workload_registry,
        // Every mode, so a device can enroll before `paired` is turned on.
        Some(env.workload_paths()),
    ) {
        Ok(quic) => {
            let bound = quic.local_addr().map_or(addr_s, |a| a.to_string());
            info!(
                addr = %bound,
                tokens = token_count,
                secure,
                workload_mtls = workload_ca.is_some(),
                "QUIC listening"
            );
            (Some(quic), RemoteListenerSlot::bound(QUIC, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind QUIC; UDS only");
            disabled(QUIC, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// Build the optional WebTransport listener for `addr`: HTTP/3 over QUIC,
/// with the same certificate, token, and loopback rules as QUIC. Browsers,
/// which cannot set headers, carry the token as `?token=` inside TLS.
#[cfg(feature = "webtransport")]
pub(in crate::runtime) fn build_wt_listener(
    addr: SocketAddr,
    workload_mtls: bool,
    env: &ServerEnv,
) -> (
    Option<crate::transport::webtransport::WtListener>,
    RemoteListenerSlot,
) {
    const WT: RemoteListenerTransport = RemoteListenerTransport::Wt;
    let secure = secure_bind(addr, env);
    let addr_s = addr.to_string();
    let Some((cert_path, key_path)) = remote_certificate(addr, "webtransport", env) else {
        return disabled(WT, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let tokens = remote_tokens(secure, "webtransport", env);
    let token_count = tokens.as_ref().map_or(0, |s| s.len());
    let paired = workload_mtls;
    let inner = inner_tls_for(paired, env, &cert_path, &key_path);
    if paired && inner.is_none() {
        error!(
            "webtransport disabled: the end-to-end TLS it needs under `paired` could not be built"
        );
        return disabled(WT, addr_s, ListenerDisabledReason::TlsSetupFailed);
    }
    match crate::transport::webtransport::WtListener::from_pem(
        addr, &cert_path, &key_path, tokens, inner,
    ) {
        Ok(wt) => {
            let bound = wt.local_addr().map_or(addr_s, |a| a.to_string());
            info!(addr = %bound, tokens = token_count, secure, "WebTransport listening");
            (Some(wt), RemoteListenerSlot::bound(WT, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind WebTransport; UDS only");
            disabled(WT, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// The end-to-end TLS session every bridged consumer runs (ADR-0180):
/// the server's own certificate and, under `paired`, the workload CA and
/// registry it requires a certificate from. `None`, after logging, when it
/// cannot be built.
pub(in crate::runtime) fn connector_inner_tls(
    paired: bool,
    env: &ServerEnv,
) -> Option<std::sync::Arc<crate::transport::inner_tls::InnerTls>> {
    let unspecified = SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0));
    let (cert, key) = remote_certificate(unspecified, "relay", env)?;
    inner_tls_for(paired, env, &cert, &key)
}

/// The end-to-end session over the server's certificate at `cert`/`key`,
/// requiring a workload certificate under `paired` (ADR-0154 items 5, 6).
pub(in crate::runtime) fn inner_tls_for(
    paired: bool,
    env: &ServerEnv,
    cert: &Path,
    key: &Path,
) -> Option<std::sync::Arc<crate::transport::inner_tls::InnerTls>> {
    let workload = match workload_auth::WorkloadAuth::for_posture(paired, env) {
        Ok(workload) => workload,
        Err(err) => {
            error!(error = %err, "configured workload mTLS unavailable for relay consumers");
            return None;
        }
    };
    match crate::transport::inner_tls::InnerTls::new(
        cert,
        key,
        workload
            .as_ref()
            .map(|auth| (&auth.ca, std::sync::Arc::clone(&auth.registry))),
        env.workload_paths(),
    ) {
        Ok(inner) => Some(std::sync::Arc::new(inner)),
        Err(err) => {
            error!(error = %err, "end-to-end TLS for relay consumers unavailable");
            None
        }
    }
}
