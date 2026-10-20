//! Shared fixtures for the dispatcher test suites.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::ModSet;
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::directory_picker::{DirectorySupport, PendingDirectory};
use crate::attach::focus::FocusHistory;
use crate::attach::input_replay::InputReplayJournal;
use crate::attach::paint::SidebarReservation;
use crate::attach::pane_state::{AttachKernel, AttentionNavigation, PaneSlot, VcsIndex};
use crate::attach::plugin_actions::PluginActionEntry;
use crate::attach::plugin_panes::PluginPaneEntry;
use crate::layout::{ArrangementDoc, SplitDir};
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::SidebarTargets;
use crate::render::chrome::status_bar::{Position, StatusBarPainter};
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::ctx::{DispatchCtx, DragGrab};
use super::deferred::DeferredArrangementEdit;
use super::dispatch::dispatch_input_events;
use super::effects::{ActionEffects, PendingSessionRename, ReattachTarget, apply_action_effects};
use super::run_action::run_action;

// Keep the test namespace and snapshot identity while grouping by responsibility.
include!("test_support/context_fixture.rs");
include!("test_support/dispatch_env.rs");
