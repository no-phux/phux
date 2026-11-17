//! The `[[connector]]` entry a relay link dials (`phux pair --relay-route`).

use phux_client_runtime::pairing::validate_quic_endpoint;

use super::RelayRoute;

/// The validated route and the `[[connector]]` entry its link dials.
pub(super) fn relay_connector(
    relay: &RelayRoute,
) -> Result<phux_config::ConnectorConfigEntry, String> {
    phux_dial::relay::validate_route_name(&relay.route)
        .map_err(|err| format!("--relay-route: {err}"))?;
    let config = phux_config::loader::load().map_err(|err| {
        format!(
            "could not read the phux config {}: {err}",
            phux_config::loader::config_path().display()
        )
    })?;
    let connector = select_relay_connector(config.connector, relay.relay.as_deref())?;
    // The link must parse on the device: refuse a relay address it could not
    // dial before anything is minted.
    validate_quic_endpoint(&format!("quic://{}", connector.relay)).map_err(|err| {
        format!(
            "the [[connector]] relay {:?} cannot go in a link ({err}); none was minted",
            connector.relay
        )
    })?;
    Ok(connector)
}

/// Pick the `[[connector]]` entry a relay link dials: the one `--relay`
/// names, else the only one. Nothing configured, or an ambiguous choice, is
/// a refusal: a link must name the relay this server actually tunnels to.
pub(super) fn select_relay_connector(
    configured: Vec<phux_config::ConnectorConfigEntry>,
    relay: Option<&str>,
) -> Result<phux_config::ConnectorConfigEntry, String> {
    let names = || {
        configured
            .iter()
            .map(|entry| entry.relay.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(relay) = relay {
        let listed = names();
        return configured
            .into_iter()
            .find(|entry| entry.relay == relay)
            .ok_or_else(|| {
                format!(
                    "no [[connector]] entry dials relay {relay:?} (configured: {}), so none was minted",
                    if listed.is_empty() { "none" } else { &listed }
                )
            });
    }
    if configured.len() > 1 {
        return Err(format!(
            "several [[connector]] relays are configured ({}); pass --relay HOST:PORT naming \
             the one this route is enrolled on",
            names()
        ));
    }
    configured.into_iter().next().ok_or_else(|| {
        "this server has no [[connector]] relay configured, so a relay link would route to \
         nothing and none was minted.\n  \
         enroll the route with `phux relay pair --route ROUTE` on the relay host, add a \
         [[connector]] entry naming that relay, and restart the server"
            .to_owned()
    })
}
