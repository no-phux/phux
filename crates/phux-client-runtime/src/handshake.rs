//! `HELLO` and its acceptance, once for every client lane.
//!
//! The reconnecting control plane ([`crate::control::ControlPlane`]) and the
//! one-shot [`crate::session::Session`] both open with [`hello`] and turn the
//! server's `HELLO_OK` into a [`ServerInfo`] through [`HelloOk::accept`], so
//! the two cannot disagree about which handshakes they take. The acceptance
//! rule itself is `phux_client_core::handshake::validate_hello_ok`, shared
//! with every tokio-free frontend.

use phux_client_core::handshake::{HelloOkError, validate_hello_ok};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ClientPlatform, LayerSet,
    ServerCapabilities, ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
};
use phux_protocol::wire::frame::FrameKind;

// ponytail: process-wide, because a process runs on one device. Make it a
// connection option if one process ever speaks for two.
static PLATFORM: std::sync::Mutex<Option<ClientPlatform>> = std::sync::Mutex::new(None);

/// Report `platform` in every later `HELLO` of this process instead of the
/// build's own OS word (an iPad build is `ios` to the compiler). `None`
/// restores the default.
pub fn set_platform(platform: Option<ClientPlatform>) {
    *PLATFORM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = platform;
}

/// The `HELLO` a client opens every connection with: this build's protocol
/// triple and the capabilities it offers.
///
/// An offer without a platform gets [`set_platform`]'s, else
/// [`ClientPlatform::host`].
#[must_use]
pub fn hello(client_name: String, client_caps: ClientCapabilities) -> FrameKind {
    let platform = client_caps
        .platform
        .or_else(|| {
            *PLATFORM
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .or(ClientPlatform::host());
    FrameKind::Hello {
        client_name,
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: client_caps.with_platform(platform),
    }
}

/// A `HELLO_OK`'s fields, as the server sent them.
#[derive(Debug, Clone)]
pub struct HelloOk {
    /// The protocol triple the server selected.
    pub protocol: (u16, u16, u16),
    /// The server's incarnation identity (ADR-0053).
    pub server_id: Vec<u8>,
    /// What the server advertised.
    pub server_caps: ServerCapabilities,
    /// The bootstrap profile the server selected. This client offers
    /// Terminal profiles, so a server that selects none broke the offer.
    pub profile: Option<BootstrapProfile>,
    /// The payload limits the server selected.
    pub limits: BootstrapLimits,
}

impl HelloOk {
    /// Accept this answer only on the terms `offered` advertised.
    ///
    /// # Errors
    ///
    /// The [`HelloOkError`] naming the term the server broke.
    pub fn accept(self, offered: &ClientCapabilities) -> Result<ServerInfo, HelloOkError> {
        let (major, minor, patch) = self.protocol;
        let profile = self.profile.ok_or(HelloOkError::NoProfile)?;
        validate_hello_ok(offered, major, minor, patch, profile, self.limits)?;
        Ok(ServerInfo {
            id: self.server_id,
            features: self.server_caps.features,
            features_ext: self.server_caps.features_ext,
            layers: self.server_caps.layers,
            protocol: self.protocol,
            profile,
            limits: self.limits,
        })
    }
}

/// What `HELLO_OK` negotiated.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// The server's incarnation identity.
    pub id: Vec<u8>,
    /// The features the server advertised.
    pub features: ServerFeatureSet,
    /// Capabilities from the trailing extension word (absent on older servers).
    pub features_ext: ServerFeatureExtSet,
    /// The layers the server advertised.
    pub layers: LayerSet,
    /// The server's protocol triple from `HELLO_OK`. Any minor or patch on
    /// this client's major is accepted, so this may be older or newer than
    /// [`PROTOCOL_VERSION`]; degrade by it when no feature bit names the
    /// behavior.
    pub protocol: (u16, u16, u16),
    /// The bootstrap profile the server selected.
    pub profile: BootstrapProfile,
    /// The payload limits the server selected.
    pub limits: BootstrapLimits,
}

impl ServerInfo {
    /// Whether the server advertised `feature`.
    #[must_use]
    pub const fn has(&self, feature: ServerFeature) -> bool {
        self.features.contains(feature)
    }

    /// Whether the server advertised an extended feature.
    #[must_use]
    pub const fn has_ext(&self, feature: ServerFeatureExt) -> bool {
        self.features_ext.contains(feature)
    }

    /// The synchronization terms a frame decoder and a replica follow.
    #[must_use]
    pub const fn bootstrap(&self) -> NegotiatedBootstrap {
        NegotiatedBootstrap {
            profile: self.profile,
            limits: self.limits,
            server_features: self.features,
            server_features_ext: self.features_ext,
        }
    }
}

/// Immutable result of protocol-0.7 bootstrap negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NegotiatedBootstrap {
    /// Exact synchronization profile selected by the server.
    pub profile: BootstrapProfile,
    /// Exact per-frame bootstrap/history payload bounds.
    pub limits: BootstrapLimits,
    /// Additive server features authenticated by `HELLO_OK`.
    pub server_features: ServerFeatureSet,
    /// Extended additive features, negotiated in the trailing `HELLO_OK` word.
    pub server_features_ext: ServerFeatureExtSet,
}

#[cfg(test)]
mod tests {
    use phux_protocol::caps::{BootstrapCapabilities, select_bootstrap_profile};

    use super::*;

    fn answer(offered: &ClientCapabilities, protocol: (u16, u16, u16)) -> HelloOk {
        let (profile, limits) = select_bootstrap_profile(offered, &BootstrapCapabilities::new())
            .expect("the default offers intersect");
        HelloOk {
            protocol,
            server_id: b"incarnation".to_vec(),
            server_caps: ServerCapabilities::new(),
            profile: Some(profile),
            limits,
        }
    }

    #[test]
    fn an_answer_on_the_offered_terms_becomes_the_server_info() {
        let offered = ClientCapabilities::new();
        let current = (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        );
        let info = answer(&offered, current)
            .accept(&offered)
            .expect("accepted");
        assert_eq!(info.id, b"incarnation");
        assert_eq!(info.protocol, current);
        assert_eq!(info.bootstrap().limits, info.limits);
    }

    #[test]
    fn a_newer_minor_on_the_same_major_is_accepted_and_recorded() {
        let offered = ClientCapabilities::new();
        let newer = (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor.wrapping_add(1),
            PROTOCOL_VERSION.patch.wrapping_add(3),
        );
        let info = answer(&offered, newer)
            .accept(&offered)
            .expect("same major interoperates");
        assert_eq!(info.protocol, newer, "the server's version, not ours");
    }

    #[test]
    fn another_protocol_major_is_refused() {
        let offered = ClientCapabilities::new();
        let skewed = (
            PROTOCOL_VERSION.major.wrapping_add(1),
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        );
        assert!(matches!(
            answer(&offered, skewed).accept(&offered),
            Err(HelloOkError::UnsupportedProtocol { .. })
        ));
    }

    #[test]
    fn hello_carries_this_builds_protocol_and_the_offer() {
        let caps = ClientCapabilities::new().with_quic_streams(true);
        let FrameKind::Hello {
            client_name,
            protocol_major,
            protocol_patch,
            client_caps,
            ..
        } = hello("probe".to_owned(), caps)
        else {
            panic!("hello builds HELLO");
        };
        assert_eq!(client_name, "probe");
        assert_eq!(protocol_major, PROTOCOL_VERSION.major);
        assert_eq!(protocol_patch, PROTOCOL_VERSION.patch);
        assert_eq!(client_caps, caps.with_platform(ClientPlatform::host()));
        assert!(
            client_caps.platform.is_some(),
            "this build's OS is reported"
        );
    }
}
