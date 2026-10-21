//! What a consumer configures the control plane with.

use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapLimits, ClientCapabilities, ImageProtocolSet, Layer, LayerSet,
};
#[cfg(not(feature = "engine"))]
use phux_protocol::caps::{BootstrapProfileKind, BootstrapProfileSet};
use phux_protocol::wire::frame::{AttachTarget, RolePolicy};

/// What a consumer asks the control plane to do on every connection.
#[derive(Debug, Clone)]
pub struct ControlOptions {
    /// The `HELLO` client name.
    pub client_name: String,
    /// Whether the runtime queues its post-handshake attach, topology reads,
    /// and agent metadata discovery/recovery.
    ///
    /// Socket-owning embedders set this to `false`: their stable ABI exposes
    /// `HELLO` and `ATTACH` as explicit calls, while this plane still owns all
    /// lifecycle validation and frame application. These embedders also own
    /// request-ID allocation and metadata orchestration; autonomous roster
    /// reads would collide with their externally allocated correlations.
    pub automatic_lifecycle: bool,
    /// Recover session selection and terminal subscriptions after transport loss.
    /// Disable when the consumer revalidates authority and persistent keys from
    /// fresh inventory before explicitly selecting work and rebuilding views.
    pub session_recovery: SessionRecovery,
    /// The viewport `ATTACH` and spawns declare, in cells. `(0, 0)` casts no
    /// vote: the server ignores it in window-size policy, a session attach
    /// resizes nothing, and every terminal's size is set explicitly (a GUI
    /// whose panes each size their own terminal).
    pub viewport: (u16, u16),
    /// The scrollback depth `ATTACH` requests and the history cache keeps.
    pub scrollback_lines: u32,
    /// The session every connection attaches after `HELLO_OK`; `None`
    /// browses the topology (`GET_STATE`) without attaching.
    pub attach: Option<AttachTarget>,
    /// The role every `ATTACH` declares (ADR-0127); `None` sends nothing.
    pub attach_role: Option<RolePolicy>,
    /// The journal cursor the automatic `SUBSCRIBE_EVENTS` carries on a
    /// server that advertises `EVENT_JOURNAL`; `None` is live-only.
    pub event_after_seq: Option<u64>,
    /// Pick up a terminal another client spawned with a per-terminal
    /// attach on the live socket, so its output reaches this client
    /// without a reconnect.
    pub auto_attach_foreign_spawns: bool,
    /// Stream every `AgentSession` resource (ADR-0103) whose parent Terminal
    /// this connection streams: subscribe it with a per-resource attach,
    /// re-read the topology when one is announced, and surface its commits
    /// as [`Event::AgentPublication`] and its end as
    /// [`Event::AgentSessionClosed`]. Needs `automatic_lifecycle`, which
    /// owns the request ids those attaches use.
    pub subscribe_agent_sessions: bool,
    /// The bootstrap payload limits offered in `HELLO`.
    pub bootstrap_limits: BootstrapLimits,
    /// Where the connection driver puts an inbound frame.
    pub deliver_inbound: InboundDelivery,
}

/// Who re-establishes session and terminal admission after transport loss.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionRecovery {
    /// Follow the confirmed session and replay preserved terminal subscriptions.
    #[default]
    Automatic,
    /// Read inventory only. The caller validates authority and persistent keys,
    /// then explicitly selects work and re-establishes its views.
    CallerValidated,
}

/// What a connected driver does with a frame it read off the socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InboundDelivery {
    /// Feed it straight into this plane, on the driver's thread. The
    /// consumer observes the result through events and the published grid.
    #[default]
    Fed,
    /// Queue it for the consumer to drain with `take_inbound` and feed
    /// itself. A binding whose own per-frame state lives on one owning
    /// thread needs this: it keeps the socket in the runtime and the
    /// decode point in the binding.
    Queued,
}

/// The inbound queue's ceiling. At it the driver stops reading, so a
/// consumer that falls behind backs the socket up to the server instead
/// of growing the queue or failing the connection.
pub const MAX_QUEUED_INBOUND_FRAMES: usize = 128;
/// The inbound queue's byte ceiling; see [`MAX_QUEUED_INBOUND_FRAMES`].
///
/// It counts frame bytes. A queued frame is a view of the driver's read
/// buffer, not a copy, so it also pins that read's slab (64 KiB, or one
/// larger frame) until the consumer drains it.
pub const MAX_QUEUED_INBOUND_BYTES: usize = 32 * 1024 * 1024;

impl Default for ControlOptions {
    fn default() -> Self {
        Self {
            client_name: "phux-client-runtime".to_owned(),
            automatic_lifecycle: true,
            session_recovery: SessionRecovery::Automatic,
            viewport: (80, 24),
            scrollback_lines: 1000,
            attach: None,
            attach_role: None,
            event_after_seq: None,
            auto_attach_foreign_spawns: true,
            subscribe_agent_sessions: false,
            bootstrap_limits: BootstrapLimits::default(),
            deliver_inbound: InboundDelivery::Fed,
        }
    }
}

impl ControlOptions {
    /// The capabilities `HELLO` advertises: every bootstrap profile the
    /// hosted engine can consume, and L3 metadata.
    #[must_use]
    #[allow(
        clippy::missing_const_for_fn,
        reason = "the engine offer probes libghostty at runtime; only the headless offer is constant"
    )]
    pub fn client_caps(&self) -> ClientCapabilities {
        ClientCapabilities::new()
            // GridFrame publishes text/style POD cells, not image planes. Do
            // not pay mobile bandwidth for escapes no runtime consumer sees.
            .with_image_protocols(ImageProtocolSet::new())
            .with_layers(LayerSet::with(&[Layer::L3]))
            .with_bootstrap(self.bootstrap_caps())
    }

    #[cfg(feature = "engine")]
    fn bootstrap_caps(&self) -> BootstrapCapabilities {
        phux_client_core::engine::ghostty::native_bootstrap_capabilities(self.bootstrap_limits)
    }

    #[cfg(not(feature = "engine"))]
    const fn bootstrap_caps(&self) -> BootstrapCapabilities {
        BootstrapCapabilities::new()
            .with_profiles(BootstrapProfileSet::with(&[
                BootstrapProfileKind::SynthesizedVtRaw,
            ]))
            .with_limits(self.bootstrap_limits)
    }
}
