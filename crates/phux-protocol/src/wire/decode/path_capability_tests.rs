use super::*;
use crate::caps::{ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet};

#[test]
fn second_word_preserves_first() {
    let legacy = decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0])).expect("old caps");
    assert_eq!(legacy.features, ServerFeatureSet::new());
    assert_eq!(legacy.features_ext, ServerFeatureExtSet::new());
    let extended = decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0x80, 0, 0, 1]))
        .expect("extended caps");
    assert_eq!(extended.features, ServerFeatureSet::new());
    assert!(extended.features_ext.contains(ServerFeatureExt::PathQuery));
    assert!(extended.features_ext.contains(ServerFeatureExt::OpenUrl));
    assert_eq!(extended.features_ext.as_wire(), 0x8000_0001);
    assert!(decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0])).is_err());
}

#[test]
fn third_word_is_explicit_policy_proof_and_old_caps_never_imply_it() {
    use crate::caps::{ServerFeatureExt2, ServerFeatureExt2Set};
    for old in [
        &[1][..],
        &[1, 0, 0, 0, 0][..],
        &[1, 0, 0, 0, 0, 0, 0, 0, 0][..],
    ] {
        let caps = decode_server_capabilities(&mut Decoder::new(old)).unwrap();
        assert_eq!(caps.features_ext2, ServerFeatureExt2Set::new());
    }
    let caps = decode_server_capabilities(&mut Decoder::new(&[
        1, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0, 0, 1,
    ]))
    .unwrap();
    assert!(
        caps.features_ext2
            .contains(ServerFeatureExt2::ArchivedLaunchAdmission)
    );
    assert_eq!(caps.features_ext2.as_wire(), 1, "unknown bits stay inert");
    assert!(
        decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 1,])).is_err(),
        "a partial third word cannot claim enforcement"
    );
}
