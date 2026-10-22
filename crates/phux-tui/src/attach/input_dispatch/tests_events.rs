//! Event-routing tests: overlay key interception, sidebar and bar
//! clicks, mouse forwarding, set-pane, and predictive echo gating.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::HashSet;

use phux_config::vocab::ACTION_NAMES;
use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton};
use phux_protocol::wire::frame::{Command, FrameKind};

use crate::attach::input_replay::{InputReplayJournal, ReplayDisposition};
use crate::attach::paint::{SidebarEdge, SidebarReservation, content_rect};
use crate::attach::render::ReplicaWalk;
use crate::layout::Rect;
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::Theme;
use crate::render::chrome::sidebar::SidebarTarget;
use crate::render::chrome::status_bar::{BarInset, Position, StatusBarPainter, make_context};

use super::args::*;
use super::ctx::*;
use super::dispatch::*;
use super::test_support::*;

// Keep the test namespace and snapshot identity while grouping by responsibility.
include!("tests_events/overlays.rs");
include!("tests_events/sidebar.rs");
include!("tests_events/status_bar.rs");
include!("tests_events/pane_mouse.rs");
include!("tests_events/input_delivery_prediction.rs");
include!("tests_events/floating.rs");
include!("tests_events/layout_read.rs");
include!("tests_events/key_modes.rs");
