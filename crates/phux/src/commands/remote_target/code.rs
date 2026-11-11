//! Registration and optional workload enrollment from a pasted connect code.

use super::{RemoteEntry, RemoteTarget, registered_entry};
use crate::commands::{enroll, pair, remote};

/// Register a host from a pasted connect link and return the entry to dial.
pub(super) fn register_from_code(target: &RemoteTarget, code: &str) -> Result<RemoteEntry, String> {
    let link = pair::parse_connect_link(code).map_err(|err| format!("--code: {err}"))?;

    let name = target.registry_name();
    let token_file = enroll::token_path(&phux_server::telemetry::state_dir(), &name);

    // A relay link registers the relay with its route (ADR-0149); any other
    // prefers its supplied WebSocket url, with its QUIC route kept beside it
    // for attach to promote once it answers. A QUIC-only code registers QUIC.
    let endpoint = link
        .registry_endpoint()
        .ok_or_else(|| "--code: the connect code names no endpoint".to_owned())?;
    // Validate before the token lands: a rejected name or an unpinned
    // routable endpoint must not leave an orphaned bearer token on disk.
    // Same ordering `phux host add` uses, and for the same reason.
    let new = remote::NewRemote::new(
        &name,
        endpoint,
        Some(&token_file),
        link.cert_fingerprint.as_deref(),
        None,
    )
    .and_then(|new| new.with_tls_server_name(link.tls_server_name.as_deref()))
    .and_then(|new| new.with_direct(link.quic.as_deref()))
    .map_err(|err| format!("--code: {err}"))?;
    let identity = enroll_from_code(&name, &link);
    let new = new
        .with_client_identity(
            identity
                .as_ref()
                .map(|files| (files.certificate.as_path(), files.private_key.as_path())),
        )
        .map_err(|err| format!("--code: {err}"))?;

    enroll::write_token(&token_file, &link.token).map_err(|err| format!("--code: {err}"))?;
    remote::add_or_update(&new).map_err(|err| format!("--code: {err}"))?;
    // A relay link pins the relay's leaf, and its `ca` is the server's,
    // which the end-to-end session inside the relayed stream verifies
    // (ADR-0154): keyed by the route beside the relay's leaf.
    remote::pin_authority(
        &name,
        link.cert_fingerprint.as_deref(),
        link.tls_server_name.as_deref(),
        link.ca_fingerprint.as_deref(),
    );

    ok!("paired {name} -> {} (from the connect code)", new.endpoint);
    hint!("next time, `phux --remote {name}` needs no code");

    Ok(registered_entry(target, &new))
}

/// Enroll a workload certificate with the link's single-use ticket, when it
/// carries one (ADR-0154). A failure is a warning, as for `host add`: the
/// host is still registered, though a `paired` server, the only kind with a
/// remote listener (ADR-0179), admits no dial without a certificate.
fn enroll_from_code(name: &str, link: &pair::ConnectLink) -> Option<enroll::ClientIdentityFiles> {
    let ticket = link.enrollment_ticket.as_deref()?;
    let Some(quic) = link
        .quic
        .as_deref()
        .and_then(|quic| quic.strip_prefix("quic://"))
    else {
        warn!("{name}: the link carries an enrollment ticket but no quic endpoint to enroll over");
        return None;
    };
    let remotes_dir = phux_server::telemetry::state_dir().join("remotes");
    let pins = enroll::TicketPins {
        leaf: link.cert_fingerprint.as_deref(),
        authority: link.ca_fingerprint.as_deref(),
        route: link.tls_server_name.as_deref(),
    };
    let workload = enroll::WorkloadEnrollment {
        dir: &remotes_dir,
        name,
    };
    match enroll::enroll_with_ticket(quic, &pins, ticket, &workload) {
        Ok(files) => {
            ok!(
                "{name}: enrolled a workload certificate ({})",
                files.credential_id.as_deref().unwrap_or("unknown id")
            );
            Some(files)
        }
        Err(err) => {
            warn!(
                "{name}: could not enroll a workload certificate: {err}; \
                 a server in `paired` mode refuses this host until it does (mint a new link with \
                 `phux pair --enroll` there)"
            );
            None
        }
    }
}
