use super::*;
use crate::attach::pane_state::PaneSlot;
use crate::layout::ArrangementDoc;
use phux_client::agent_meta::AgentRecord;
use phux_protocol::WindowId;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerFeature, ServerFeatureSet};
use phux_protocol::ids::SessionId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::wire::frame::SESSION_NAME_KEY;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

#[path = "loop_state_tests/agent_streams.rs"]
mod agent_streams;

/// A fresh raw synthesized-VT attach loop over one end of a socket pair.
///
/// The server end stays a raw socket: each test wraps it in a
/// [`Connection`], reads it as bytes, or holds it unread.
struct LoopRig {
    state: SessionLoop,
    client: Connection,
    server: tokio::net::UnixStream,
    out: Vec<u8>,
}

impl LoopRig {
    /// A loop on a server advertising `features` and `features_ext`.
    fn new(features: ServerFeatureSet, features_ext: phux_protocol::ServerFeatureExtSet) -> Self {
        let (client, server) = tokio::net::UnixStream::pair().unwrap();
        let negotiated = NegotiatedBootstrap {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features: features,
            server_features_ext: features_ext,
            server_features_ext2: phux_protocol::caps::ServerFeatureExt2Set::new(),
        };
        let state = SessionLoop::new(
            negotiated,
            PredictiveConfig::disabled(),
            false,
            None,
            EntryPick::default(),
            None,
        )
        .unwrap();
        Self {
            state,
            client: Connection::from_stream(client),
            server,
            out: Vec::new(),
        }
    }

    /// A loop on a server advertising nothing.
    fn plain() -> Self {
        Self::new(
            ServerFeatureSet::new(),
            phux_protocol::ServerFeatureExtSet::new(),
        )
    }

    /// The loop bootstrapped from `attached`.
    async fn bootstrapped(mut self, attached: FrameKind) -> Self {
        self.state
            .bootstrap(&mut self.client, &mut self.out, attached, None)
            .await
            .unwrap();
        self
    }
}

/// The shipped default settings, as a config with no overrides reads them.
fn use_shipped_settings(state: &mut SessionLoop) {
    let config = phux_config::parse_with_defaults("", std::path::Path::new("t.toml"))
        .expect("shipped defaults parse");
    state.settings = crate::settings::TuiSettings::tolerant_from(&config);
}

/// Publish `seeds` as the mirror's kernel and pane slots.
fn publish_panes(state: &mut SessionLoop, seeds: &[(&ResourceId, u16, u16, &[u8])]) {
    let (kernel, _effects, panes) = crate::attach::pane_state::published_test_state(seeds);
    state.mirror.engine_kernel = kernel;
    state.mirror.panes = panes;
}

/// Publish one pane of `bytes` and show it alone and focused, with the
/// layout read settled.
fn show_alone(state: &mut SessionLoop, pane: &ResourceId, size: (u16, u16), bytes: &[u8]) {
    publish_panes(state, &[(pane, size.0, size.1, bytes)]);
    state.mirror.workspace = ArrangementDoc::single(pane.clone());
    state.mirror.focused_resource = Some(pane.clone());
    state.layout_read_complete = true;
}

/// A key press for `spec`, as the attach parser decodes it.
fn chord_press(spec: &str) -> InputEvent {
    let chord = phux_config::keybind::parse_chord(spec).expect("chord");
    InputEvent::Key(crate::test_util::press_mods(chord.key, chord.modifiers))
}

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
include!("loop_state_tests/newer_server.rs");
include!("loop_state_tests/surface_leaves.rs");
include!("loop_state_tests/kitty_heal.rs");
include!("loop_state_tests/mouse_hover.rs");
include!("loop_state_tests/agent_forward.rs");
