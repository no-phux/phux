//! Registration and optional workload enrollment from a pasted connect code.
//!
//! `phux attach --remote NAME --code LINK` registers even when the link's
//! enrollment ticket does not redeem, and says so; `phux host add NAME --code
//! LINK` (what Phux Rune runs for a link it is handed) refuses instead, so a
//! spent or expired link never leaves a host that cannot be dialed.

use super::{RemoteEntry, RemoteTarget, find_entry};
use crate::commands::enroll::{self, CertificateStatus, ClientIdentityFiles};
use crate::commands::{pair, remote};

/// Register a host from a pasted connect link and return the entry to dial.
pub(super) fn register_from_code(target: &RemoteTarget, code: &str) -> Result<RemoteEntry, String> {
    let link = pair::parse_connect_link(code).map_err(|err| format!("--code: {err}"))?;
    let name = target.registry_name();
    let new = validated(&name, &link)?;
    // A failure is a warning, as for `host add` over ssh: the host is still
    // registered, though a `paired` server, the only kind with a remote
    // listener (ADR-0179), admits no dial without a certificate.
    let identity = match redeem_ticket(&name, &link) {
        Ok(identity) => identity,
        Err(err) => {
            warn!(
                "{name}: could not enroll a workload certificate: {err}; \
                 a server in `paired` mode refuses this host until it does (mint a new link with \
                 `phux pair --enroll` there)"
            );
            None
        }
    };
    if let Some(files) = &identity {
        ok!(
            "{name}: enrolled a workload certificate ({})",
            files.credential_id.as_deref().unwrap_or("unknown id")
        );
    }
    let new = save(new, &link, identity.as_ref())?;
    ok!("paired {name} -> {} (from the connect code)", new.endpoint);
    hint!("next time, `phux --remote {name}` needs no code");
    Ok(registered_entry(target, &new))
}

/// Register a host from a connect link only once its enrollment ticket, when
/// it carries one, has redeemed. Narrates nothing: the caller reports.
pub(crate) fn pair_from_code(
    target: &RemoteTarget,
    code: &str,
) -> Result<(RemoteEntry, CertificateStatus), String> {
    let link = pair::parse_connect_link(code).map_err(|err| format!("--code: {err}"))?;
    let name = target.registry_name();
    let new = validated(&name, &link)?;
    let identity = redeem_ticket(&name, &link).map_err(|err| {
        format!(
            "--code: could not enroll with the link's single-use ticket ({err}); \
             the link may have expired or been used already, or the machine is unreachable"
        )
    })?;
    let status = if identity.is_some() {
        CertificateStatus::Enrolled
    } else {
        CertificateStatus::Skipped
    };
    let new = save(new, &link, identity.as_ref()).inspect_err(|_| {
        // Nothing names the identity this enrolled: it must not outlive the refusal.
        if let Some(files) = &identity {
            let _ = std::fs::remove_file(&files.certificate);
            let _ = std::fs::remove_file(&files.private_key);
        }
    })?;
    Ok((registered_entry(target, &new), status))
}

/// The entry the link describes, validated before anything is enrolled or
/// written: a rejected name or an unpinned routable endpoint must not leave
/// an orphaned bearer token or identity on disk. Same ordering `phux host
/// add` uses, and for the same reason.
fn validated(name: &str, link: &pair::ConnectLink) -> Result<remote::NewRemote, String> {
    let token_file = enroll::token_path(&phux_server::telemetry::state_dir(), name);
    // A relay link registers the relay with its route (ADR-0149); any other
    // prefers its supplied WebSocket url, with its QUIC route kept beside it
    // for attach to promote once it answers. A QUIC-only code registers QUIC.
    let endpoint = link
        .registry_endpoint()
        .ok_or_else(|| "--code: the connect code names no endpoint".to_owned())?;
    remote::NewRemote::new(
        name,
        endpoint,
        Some(&token_file),
        link.cert_fingerprint.as_deref(),
        None,
    )
    .and_then(|new| new.with_tls_server_name(link.tls_server_name.as_deref()))
    .and_then(|new| new.with_direct(link.quic.as_deref()))
    .map_err(|err| format!("--code: {err}"))
}

/// Write the token and the entry, then pin the link's authority.
fn save(
    new: remote::NewRemote,
    link: &pair::ConnectLink,
    identity: Option<&ClientIdentityFiles>,
) -> Result<remote::NewRemote, String> {
    let new = new
        .with_client_identity(
            identity.map(|files| (files.certificate.as_path(), files.private_key.as_path())),
        )
        .map_err(|err| format!("--code: {err}"))?;
    let token_file = new
        .token_file
        .clone()
        .ok_or_else(|| "--code: the entry has no token file".to_owned())?;
    enroll::write_token(&token_file, &link.token).map_err(|err| format!("--code: {err}"))?;
    remote::add_or_update(&new).map_err(|err| format!("--code: {err}"))?;
    // A relay link pins the relay's leaf, and its `ca` is the server's,
    // which the end-to-end session inside the relayed stream verifies
    // (ADR-0154): keyed by the route beside the relay's leaf.
    remote::pin_authority(
        &new.name,
        link.cert_fingerprint.as_deref(),
        link.tls_server_name.as_deref(),
        link.ca_fingerprint.as_deref(),
    );
    Ok(new)
}

/// Enroll a workload certificate with the link's single-use ticket, when it
/// carries one (ADR-0154). `Ok(None)` when it carries none.
fn redeem_ticket(
    name: &str,
    link: &pair::ConnectLink,
) -> Result<Option<ClientIdentityFiles>, String> {
    let Some(ticket) = link.enrollment_ticket.as_deref() else {
        return Ok(None);
    };
    let quic = link
        .quic
        .as_deref()
        .and_then(|quic| quic.strip_prefix("quic://"))
        .ok_or_else(|| {
            "the link carries an enrollment ticket but no quic endpoint to enroll over".to_owned()
        })?;
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
    enroll::enroll_with_ticket(quic, &pins, ticket, &workload).map(Some)
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
        tls_server_name: new.tls_server_name.clone(),
        session: None,
        ssh: None,
        direct: new.direct.clone(),
        remote_phux: new.remote_phux.clone(),
        client_cert: new.client_cert.clone(),
        client_key: new.client_key.clone(),
        color: None,
        paused: false,
    })
}
