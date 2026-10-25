use super::*;
use crate::c::types::PhuxClientResult;

#[test]
fn unavailable_projection_keeps_the_c_state_error_category() {
    let error = engine_bridge(EngineError::ProjectionUnavailable);
    assert_eq!(error.result, PhuxClientResult::InvalidState);
    assert_eq!(error.message, "no projection for this terminal");
}
