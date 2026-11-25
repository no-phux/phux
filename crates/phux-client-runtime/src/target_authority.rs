//! Pairing-store authority facts translated into lower native trust planning.

use std::path::Path;

/// The server's certificate-authority pin (ADR-0153): the CA a dial requires
/// the server's leaf to chain to. It is made at pairing and never learned
/// from a connection (ADR-0179).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorityPin {
    /// The pinned CA, `sha256:` form. `None` pins the leaf alone.
    pub ca: Option<String>,
    /// The relay route the server is reached through (ADR-0154 item 5): the
    /// leaf pin is then the relay's, and [`Self::ca`] is the server's, which
    /// an end-to-end session inside the relayed stream verifies.
    pub route: Option<String>,
}

impl AuthorityPin {
    /// The pin `known-authorities` beside `config_path` holds for `leaf`
    /// (through `route` when a relay serves the server).
    #[must_use]
    pub fn from_store(config_path: &Path, leaf: Option<&str>, route: Option<&str>) -> Self {
        let route = route
            .map(str::trim)
            .filter(|route| !route.is_empty())
            .map(str::to_owned);
        let Some(leaf) = leaf.filter(|leaf| !leaf.trim().is_empty()) else {
            return Self {
                route,
                ..Self::default()
            };
        };
        let store = phux_config_core::known_authorities::path_beside(config_path);
        let ca = phux_config_core::known_authorities::lookup(&store, leaf, route.as_deref());
        Self { ca, route }
    }

    /// The dial trust for `leaf`: [`phux_dial::CertTrust::from_pins`]. For a
    /// relayed server it is the relay's leaf alone; the server's CA belongs
    /// to the end-to-end [`phux_dial::quic::InnerTls`] session.
    #[must_use]
    pub fn trust(&self, leaf: Option<&str>) -> Option<phux_dial::CertTrust> {
        phux_dial::CertTrust::from_route_pins(leaf, self.ca.as_deref(), self.route.as_deref())
    }
}
