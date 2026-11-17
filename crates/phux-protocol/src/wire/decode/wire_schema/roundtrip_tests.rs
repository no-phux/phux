//! Every frame and command in the sample corpus decodes back to itself.
//!
//! The corpus is the wire schema's [`frame_samples`] (client frames from the
//! classifier's samples plus every server frame) and the classifier's
//! per-variant [`command_samples`]. A value that encodes but decodes to
//! something else is a silent codec asymmetry, which the schema's presence
//! derivation cannot see.

use super::FrameKind;
use super::derive::decodes;
use super::samples::{encoded, frame_samples};
use crate::kinds::samples::command_samples;
use crate::wire::error::DecodeError;

/// Client frame samples the decoder refuses on purpose: the four spawns that
/// break their kind's spawn contract (a parented terminal, an orphan agent
/// session, and an agent session with an owner terminal or a retention). The
/// schema corpus drops them, so a sample that newly fails to decode must
/// change this count, not vanish.
const REFUSED_CLIENT_SAMPLES: usize = 4;

fn assert_round_trips(frame: &FrameKind) {
    let bytes = encoded(frame);
    let (decoded, tail) = FrameKind::decode(&bytes)
        .unwrap_or_else(|err| panic!("{frame:?} does not decode: {err:?}"));
    assert!(tail.is_empty(), "{frame:?} leaves {} bytes", tail.len());
    assert_eq!(&decoded, frame);
}

#[test]
fn every_frame_sample_round_trips() {
    for frame in frame_samples() {
        assert_round_trips(&frame);
    }
}

#[test]
fn every_command_sample_round_trips() {
    for (command, _) in command_samples() {
        assert_round_trips(&FrameKind::Command {
            request_id: 1,
            command,
        });
    }
}

#[test]
fn only_the_contract_breaking_spawns_are_left_out() {
    let refused: Vec<FrameKind> = crate::kinds::samples::frame_samples()
        .into_iter()
        .map(|(frame, _)| frame)
        .filter(|frame| !decodes(&encoded(frame)))
        .collect();
    assert_eq!(refused.len(), REFUSED_CLIENT_SAMPLES, "{refused:#?}");
    for frame in &refused {
        let err = FrameKind::decode(&encoded(frame)).unwrap_err();
        assert!(
            matches!(err, DecodeError::InvalidSpawnForKind { .. }),
            "{frame:?} is refused for {err:?}, not its spawn contract"
        );
    }
}
