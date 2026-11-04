use super::*;

pub(super) fn hello(organization: bool) -> FrameKind {
    let ext = if organization {
        ServerFeatureExtSet::with(&[
            ServerFeatureExt::Organization,
            ServerFeatureExt::SessionKill,
            ServerFeatureExt::SessionProject,
        ])
    } else {
        ServerFeatureExtSet::new()
    };
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::with(&[Layer::L3]))
            .with_features(ServerFeatureSet::with(&[ServerFeature::CloseTabResources]))
            .with_features_ext(ext),
        server_id: vec![0xAB; 16],
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    }
}
