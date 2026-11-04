use super::*;

pub(super) fn hello(supported: bool) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new().with_features_ext(if supported {
            ServerFeatureExtSet::with(&[
                ServerFeatureExt::Organization,
                ServerFeatureExt::SessionKill,
            ])
        } else {
            ServerFeatureExtSet::new()
        }),
        server_id: vec![0xAB; 16],
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    }
}
