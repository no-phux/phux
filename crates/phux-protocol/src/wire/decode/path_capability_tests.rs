use super::*;
use crate::caps::{ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet};

#[test]
fn second_word_preserves_first_and_ignores_unknown_bits() {
    let legacy = decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0])).expect("old caps");
    assert_eq!(legacy.features, ServerFeatureSet::new());
    assert_eq!(legacy.features_ext, ServerFeatureExtSet::new());
    let extended = decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0x80, 0, 0, 1]))
        .expect("extended caps");
    assert_eq!(extended.features, ServerFeatureSet::new());
    assert!(extended.features_ext.contains(ServerFeatureExt::PathQuery));
    assert_eq!(extended.features_ext.as_wire(), 1);
    assert!(decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0])).is_err());
}
