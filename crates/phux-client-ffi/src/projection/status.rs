//! Negotiated capabilities, as a product reads them.

use phux_client_runtime::control::ServerInfo;
use phux_protocol::caps::{ServerFeature, ServerFeatureExt};

/// The optional server features this session negotiated, named as the
/// products name them.
#[must_use]
pub fn negotiated_features(server: &ServerInfo) -> Vec<String> {
    [
        (ServerFeature::AcknowledgedInput, "acknowledged-input"),
        (ServerFeature::FileUpload, "file-upload"),
        (ServerFeature::Transcribe, "transcribe"),
        (ServerFeature::ListDirectory, "list-directory"),
    ]
    .into_iter()
    .filter(|(feature, _)| server.features.contains(*feature))
    .map(|(_, name)| name.to_owned())
    .chain(
        [
            (ServerFeatureExt::PathQuery, "path-query"),
            (ServerFeatureExt::Checkout, "checkout"),
            (ServerFeatureExt::CheckoutV2, "checkout-v2"),
            (ServerFeatureExt::HostServiceHttp, "service-http"),
            (ServerFeatureExt::Mirrors, "mirrors"),
            (ServerFeatureExt::HostChecklist, "host-checklist"),
        ]
        .into_iter()
        .filter(|(feature, _)| server.features_ext.contains(*feature))
        .map(|(_, name)| name.to_owned()),
    )
    .chain(server.features_ext2.contains(phux_protocol::caps::ServerFeatureExt2::ArchivedLaunchAdmission)
        .then(|| "archived-launch-admission".to_owned()))
    .collect()
}

/// The protocol version `server` speaks, as `major.minor.patch`.
#[must_use]
pub fn protocol_version(server: &ServerInfo) -> String {
    format!(
        "{}.{}.{}",
        server.protocol.0, server.protocol.1, server.protocol.2
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn checkout_is_named_only_when_advertised() {
        use phux_protocol::caps::{
            BootstrapLimits, LayerSet, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
        };
        let mut server = super::ServerInfo {
            id: Vec::new(),
            features: ServerFeatureSet::new(),
            features_ext: ServerFeatureExtSet::new(),
            features_ext2: phux_protocol::caps::ServerFeatureExt2Set::new(),
            layers: LayerSet::all(),
            protocol: (0, 9, 0),
            profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
        };
        assert!(super::negotiated_features(&server).is_empty());
        server.features_ext = ServerFeatureExtSet::with(&[ServerFeatureExt::Checkout]);
        assert_eq!(super::negotiated_features(&server), ["checkout"]);
    }
}
