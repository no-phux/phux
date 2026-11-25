//! Importing a phone pairing link: one complete enrollment invite, checked
//! and turned into a configuration plus its single-use ticket.

use serde_json::{Value, json};

use super::{Configuration, is_hex_secret};

/// Import one complete enrollment link without merging stored credentials.
/// Returns configuration plus ephemeral ticket and public confirmation labels.
/// No connection or enrollment starts until the native host obtains consent.
pub fn parse_pairing_invite(raw: &str) -> Result<Value, String> {
    validate_invite_input(raw)?;
    let normalized;
    let link = if let Some(query) = raw.strip_prefix("phux-rune://connect?") {
        normalized = format!("https://phux.sh/connect?{query}");
        normalized.as_str()
    } else {
        raw
    };
    let invite = phux_client_runtime::pairing::parse_connect_link(link)?;
    let configuration = invite_configuration(&invite)?;
    let ticket = invite
        .enrollment_ticket
        .as_deref()
        .ok_or("Pairing needs the single-use enrollment ticket. Generate a new phone QR.")?;
    if !is_hex_secret(ticket) {
        return Err("The enrollment ticket must contain 64 hexadecimal digits.".into());
    }
    let endpoint = configuration["endpoint"]
        .as_str()
        .ok_or("Invalid pairing endpoint.")?;
    let peer = invite.tls_server_name.as_ref().map_or_else(
        || endpoint.to_owned(),
        |route| format!("{route} via {endpoint}"),
    );
    Ok(json!({"configuration": configuration, "ticket": ticket,
        "name": invite.name, "peer": peer}))
}

fn validate_invite_input(raw: &str) -> Result<(), String> {
    if raw.len() > phux_client_runtime::pairing::MAX_CONNECT_LINK_BYTES {
        return Err("Pairing links must not exceed 16 KiB.".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("Pairing links must not contain control characters.".into());
    }
    Ok(())
}

fn invite_configuration(
    invite: &phux_client_runtime::pairing::ConnectLink,
) -> Result<Value, String> {
    let endpoint = invite
        .quic
        .as_deref()
        .ok_or("Phone pairing needs a quic:// endpoint. Generate a new phone QR.")?;
    let pin = invite
        .cert_fingerprint
        .as_deref()
        .filter(|pin| !pin.is_empty())
        .ok_or("Pairing needs the server certificate pin.")?;
    let authority = invite
        .ca_fingerprint
        .as_deref()
        .ok_or("Pairing needs the certificate authority pin. Generate a new phone QR.")?;
    if !is_hex_secret(&invite.token) {
        return Err("The pairing token must contain 64 hexadecimal digits.".into());
    }
    let mut value = json!({"endpoint": endpoint, "certificate_pin": pin,
        "authority_ca": authority, "token": invite.token});
    if let Some(route) = &invite.tls_server_name {
        value["tls_server_name"] = json!(route);
    }
    let configuration: Configuration =
        serde_json::from_value(value.clone()).map_err(|_| "Invalid pairing configuration.")?;
    configuration.enrollment_target()?;
    Ok(value)
}
