//! State-sync tests: consumer registration, `FRAME_ACK`, tick
//! emission, loss tolerance, backpressure, the RTT-adaptive cadence,
//! the detector tick, and input-snapshot publication.

use super::test_support::*;
use super::*;

#[path = "tests_state_sync_registration.rs"]
mod registration;

#[path = "tests_state_sync_ack.rs"]
mod ack;

#[path = "tests_state_sync_bootstrap.rs"]
mod bootstrap;

#[path = "tests_state_sync_arming.rs"]
mod arming;

#[path = "tests_state_sync_loss.rs"]
mod loss;

#[path = "tests_state_sync_emission.rs"]
mod emission;

#[path = "tests_state_sync_backpressure.rs"]
mod backpressure;

#[path = "tests_state_sync_cadence.rs"]
mod cadence;

#[path = "tests_state_sync_detector.rs"]
mod detector;
