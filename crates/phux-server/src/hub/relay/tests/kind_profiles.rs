//! Content admission asks for the link's Terminal profile only of a stream
//! that needs one: a Terminal, or a resource whose kind is not yet known.

use super::*;
use phux_protocol::ids::ResourceKind;

fn content(
    kind: Option<ResourceKind>,
    profile: Option<BootstrapProfile>,
    limits: BootstrapLimits,
) -> ProxySubscription {
    let (out_tx, _out_rx) = mpsc::channel(1);
    ProxySubscription {
        resource_kind: kind,
        terminal: 7,
        client: ClientId(1),
        out_tx,
        consumer_cancel: CancellationToken::new(),
        seq: 1,
        awaits_snapshot: true,
        bootstrap_profile: profile,
        bootstrap_limits: Some(limits),
    }
}

#[test]
fn only_a_terminal_stream_needs_the_links_terminal_profile() {
    let limits = BootstrapLimits::default();
    let session = RelaySession::new_negotiated(
        host(),
        limits,
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ResourceKinds]),
    );
    let other_profiles = [None, Some(BootstrapProfile::SynthesizedVtStateSync)];
    for kind in [
        ResourceKind::AgentSession,
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
    ] {
        for profile in other_profiles {
            assert_eq!(
                session.subscription_rejection(&content(Some(kind), profile, limits)),
                None,
                "{kind:?} with downstream profile {profile:?}"
            );
        }
        // The bounds still cross the link verbatim.
        let smaller = BootstrapLimits::new(64 * 1024, 128 * 1024).expect("valid bounds");
        assert!(
            session
                .subscription_rejection(&content(Some(kind), None, smaller))
                .is_some_and(|(code, _)| code == ErrorCode::CodecUnavailable),
            "{kind:?} with smaller bounds"
        );
    }
    for kind in [Some(ResourceKind::Terminal), None] {
        for profile in other_profiles {
            assert!(
                session
                    .subscription_rejection(&content(kind, profile, limits))
                    .is_some_and(|(code, _)| code == ErrorCode::CodecUnavailable),
                "{kind:?} with downstream profile {profile:?}"
            );
        }
        assert_eq!(
            session.subscription_rejection(&content(kind, Some(native_profile()), limits)),
            None
        );
    }
}
