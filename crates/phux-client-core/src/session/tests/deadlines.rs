//! Staging lifetimes use the same injected clock for arrival and expiry.

use super::*;

#[test]
fn bootstrap_staging_lifetime_is_checked_before_adapter_input_and_at_protocol_ready() {
    for mode in [ReadyMode::ChunkFirst, ReadyMode::ProtocolFirst] {
        let mut rig = Rig::new(mode);
        let g = gen_of(1, 1, 1);
        rig.k
            .update_at(0, g.begin(11), &mut rig.fx)
            .expect("begin staging at the deadline test's clock origin");
        let late = MAX_BOOTSTRAP_STAGING_IDLE_MS + 1;
        let input = match mode {
            ReadyMode::ChunkFirst => g.chunk(0, b"not-applied"),
            ReadyMode::ProtocolFirst => g.ready(None),
        };
        let error = rig
            .k
            .update_at(late, input, &mut rig.fx)
            .expect_err("expired staging must fail");
        assert!(matches!(error, KernelError::BootstrapStagingLimitExceeded));
        assert!(rig.k.staging(&g.id).is_none());
        assert_eq!(rig.tomb(&g), codec_failure(11));
    }
}

#[test]
fn silent_bootstrap_staging_expires_on_driver_tick() {
    let mut rig = Rig::new(ReadyMode::ProtocolFirst);
    let first = gen_of(1, 1, 1);
    let second = gen_of(2, 1, 1);
    rig.k
        .update_at(0, first.begin(7), &mut rig.fx)
        .expect("begin first staging generation");
    rig.k
        .update_at(0, second.begin(9), &mut rig.fx)
        .expect("begin second staging generation");

    assert_eq!(
        rig.k
            .expire_bootstrap_staging(MAX_BOOTSTRAP_STAGING_IDLE_MS, &mut rig.fx),
        0,
        "the exact lifetime remains valid"
    );
    assert_eq!(
        rig.k
            .expire_bootstrap_staging(MAX_BOOTSTRAP_STAGING_IDLE_MS + 1, &mut rig.fx),
        2
    );
    assert!(rig.k.staging(&first.id).is_none());
    assert!(rig.k.staging(&second.id).is_none());
    let resyncs = rig.fx.as_slice().iter().filter(|effect| {
        matches!(
            effect,
            KernelEffect::Status(KernelStatus::ResyncRequired { .. })
        )
    });
    assert_eq!(resyncs.count(), 2);
}

#[test]
fn staggered_staging_expires_once_at_each_generations_own_deadline() {
    let mut rig = Rig::new(ReadyMode::ProtocolFirst);
    let first = gen_of(1, 1, 1);
    let second = gen_of(2, 1, 1);
    rig.k
        .update_at(100, first.begin(7), &mut rig.fx)
        .expect("begin first staging generation");
    rig.k
        .update_at(101, second.begin(9), &mut rig.fx)
        .expect("begin second staging generation");
    let deadline = 100 + MAX_BOOTSTRAP_STAGING_IDLE_MS;
    assert_eq!(rig.k.expire_bootstrap_staging(deadline, &mut rig.fx), 0);
    assert_eq!(rig.k.expire_bootstrap_staging(deadline + 1, &mut rig.fx), 1);
    assert!(rig.k.staging(&first.id).is_none());
    assert!(rig.k.staging(&second.id).is_some());
    assert_eq!(rig.tomb(&first), codec_failure(7));
    assert_eq!(rig.k.expire_bootstrap_staging(deadline + 2, &mut rig.fx), 1);
    assert!(rig.k.staging(&second.id).is_none());
    assert_eq!(rig.tomb(&second), codec_failure(9));
    assert_eq!(rig.k.expire_bootstrap_staging(deadline + 3, &mut rig.fx), 0);
    assert!(rig.fx.is_empty(), "no duplicate expiry effects");
}
