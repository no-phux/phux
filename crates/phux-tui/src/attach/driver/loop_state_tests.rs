use super::*;
use crate::attach::pane_state::PaneSlot;
use crate::layout::ArrangementDoc;
use phux_client::agent_meta::AgentRecord;
use phux_protocol::WindowId;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerFeatureSet};
use phux_protocol::ids::SessionId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::SESSION_NAME_KEY;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

// Keep the test namespace and snapshot identity while grouping by responsibility.
include!("loop_state_tests/layout_read.rs");
include!("loop_state_tests/bootstrap_resize.rs");
include!("loop_state_tests/sidebar_metadata.rs");
include!("loop_state_tests/satellite_attach.rs");
include!("loop_state_tests/satellite_strays.rs");
include!("loop_state_tests/session_metadata.rs");
include!("loop_state_tests/peer_arrangements.rs");
include!("loop_state_tests/review.rs");
include!("loop_state_tests/frame_outcomes.rs");
include!("loop_state_tests/layout_healing.rs");
include!("loop_state_tests/repaint.rs");
include!("loop_state_tests/input_effects.rs");
include!("loop_state_tests/view_switch.rs");
include!("loop_state_tests/arrangement_recovery.rs");
