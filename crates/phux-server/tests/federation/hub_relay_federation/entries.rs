//! The `[[satellites]]` entry the loopback hub tests point at a satellite.

use phux_config::SatelliteConfigEntry;

pub(super) fn satellite_entry(name: &str, port: u16) -> SatelliteConfigEntry {
    SatelliteConfigEntry {
        name: name.to_owned(),
        endpoint: format!("ws://127.0.0.1:{port}"),
        enabled: true,
        token_file: None,
        cert_fingerprint: None,
        client_cert: None,
        client_key: None,
        color: None,
    }
}
