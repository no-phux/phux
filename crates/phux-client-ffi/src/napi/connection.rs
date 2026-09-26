//! Negotiation snapshot and identity fencing. `Client::with_control` keeps the
//! incarnation, epoch and readiness check on one atomic runtime snapshot.
//!
//! Registered remote dialing must use `Target::resolve(raw, config_path)`.
//! The parent-owned `mod.rs` connect path first needs one shared helper taking
//! `(Target, ClientOptions, on_activity)` so both local and registered-target
//! methods install the same wake and call the same registry session.connect.
//! No second listener, retry ladder, registry parser or ad-hoc URL dial lives here.

use ::napi::{Error, Result};
use napi_derive::napi;
use phux_client_runtime::control::{ControlPlane, ServerInfo};
use phux_protocol::caps::BootstrapProfile;

use super::DesktopClient;
use crate::projection::status;

#[napi(object)]
#[derive(Debug, Clone)]
pub struct DesktopConnectionIdentity {
    pub server_id: String,
    pub connection_epoch: String,
}

#[napi(discriminant = "kind")]
#[derive(Debug)]
pub enum DesktopBootstrapProfile {
    NativeState {
        codec: u8,
        engine_feature_bits: String,
    },
    SynthesizedVtRaw {},
    SynthesizedVtStateSync {},
}

impl TryFrom<BootstrapProfile> for DesktopBootstrapProfile {
    type Error = Error;

    fn try_from(profile: BootstrapProfile) -> Result<Self> {
        match profile {
            BootstrapProfile::NativeState { codec, features } => Ok(Self::NativeState {
                codec: codec.as_wire(),
                engine_feature_bits: features.as_wire().to_string(),
            }),
            BootstrapProfile::SynthesizedVtRaw => Ok(Self::SynthesizedVtRaw {}),
            BootstrapProfile::SynthesizedVtStateSync => Ok(Self::SynthesizedVtStateSync {}),
            _ => Err(Error::from_reason("UnsupportedBootstrapProfile")),
        }
    }
}

#[napi(object)]
#[derive(Debug)]
pub struct DesktopServerInfo {
    pub server_id: String,
    pub connection_epoch: String,
    pub protocol: String,
    pub profile: DesktopBootstrapProfile,
    /// Full advertised feature set, preserving bits without a product name.
    pub feature_bits: u32,
    /// The shared projection's named product capabilities.
    pub features: Vec<String>,
    pub layer_bits: u8,
    pub max_chunk_bytes: u32,
    pub max_history_page_bytes: u32,
}

fn encode_server(server: &ServerInfo, epoch: u64) -> Result<DesktopServerInfo> {
    Ok(DesktopServerInfo {
        server_id: status::server_id(server),
        connection_epoch: epoch.to_string(),
        protocol: status::protocol_version(server),
        profile: server.profile.try_into()?,
        feature_bits: server.features.as_wire(),
        features: status::negotiated_features(server),
        layer_bits: server.layers.as_wire(),
        max_chunk_bytes: server.limits.max_chunk_bytes(),
        max_history_page_bytes: server.limits.max_history_page_bytes(),
    })
}

#[napi]
impl DesktopClient {
    /// None until `HELLO_OK` on this connection. Old server metadata is never
    /// paired with a new epoch while a reconnect is negotiating.
    #[napi]
    pub fn server_info(&self) -> Result<Option<DesktopServerInfo>> {
        self.client()?.with_control(|control| {
            if !control.handshake_ready() {
                return Ok(None);
            }
            control
                .server()
                .map(|server| encode_server(server, control.connection_epoch()))
                .transpose()
        })
    }
}

pub(super) fn require_identity(
    control: &ControlPlane,
    identity: &DesktopConnectionIdentity,
) -> Result<()> {
    if !control.handshake_ready() {
        return Err(Error::from_reason("NotNegotiated"));
    }
    let server = control
        .server()
        .ok_or_else(|| Error::from_reason("NotNegotiated"))?;
    if identity.connection_epoch != control.connection_epoch().to_string()
        || identity.server_id != status::server_id(server)
    {
        return Err(Error::from_reason("StaleConnectionIdentity"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::{BootstrapLimits, LayerSet, ServerFeatureSet};

    #[test]
    fn non_utf8_incarnation_and_high_epoch_are_lossless() {
        let server = ServerInfo {
            id: vec![0, 0xff, 0x80, 0x0a],
            features: ServerFeatureSet::default(),
            layers: LayerSet::all(),
            protocol: (1, 2, 3),
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
        };
        let dto = encode_server(&server, u64::MAX).expect("encode");
        assert_eq!(dto.server_id, "00ff800a");
        assert_eq!(dto.connection_epoch, "18446744073709551615");
        assert_eq!(dto.protocol, "1.2.3");
        assert_eq!(dto.layer_bits, server.layers.as_wire());
        assert!(matches!(
            dto.profile,
            DesktopBootstrapProfile::SynthesizedVtRaw {}
        ));
    }
}
