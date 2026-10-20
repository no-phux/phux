/// A fixture over `workspace` with request ids starting at 100.
pub(super) fn fx(workspace: ArrangementDoc) -> CtxFixture {
    CtxFixture {
        workspace,
        next_request_id: 100,
        ..CtxFixture::default()
    }
}

/// Every frame the peer received until the writer's EOF.
#[allow(clippy::future_not_send, reason = "current-thread test state")]
async fn drain(mut peer: Connection) -> Vec<FrameKind> {
    let mut received = Vec::new();
    while let Ok(frame) = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
        .await
        .expect("timed out draining the peer connection")
    {
        received.push(frame);
    }
    received
}

/// What one dispatched batch did.
pub(super) struct Sent {
    pub(super) frames: Vec<FrameKind>,
    pub(super) detach: bool,
    pub(super) repainted: bool,
}

/// Everything `dispatch_input_events` threads besides the context.
pub(super) struct Env<'a> {
    pub(super) fx: CtxFixture,
    pub(super) panes: HashMap<ResourceId, PaneSlot>,
    pub(super) focused: Option<ResourceId>,
    pub(super) predict: PredictionState,
    pub(super) resolver: Option<phux_config::keybind::Resolver>,
    pub(super) keybindings: Option<phux_config::KeybindingsCfg>,
    pub(super) journal: Option<&'a RefCell<InputReplayJournal>>,
}

impl Env<'_> {
    /// Focus on pane 1, predictor disabled, no resolver.
    pub(super) fn new(fx: CtxFixture) -> Self {
        Self {
            fx,
            panes: HashMap::new(),
            focused: Some(tid(1)),
            predict: PredictionState::new(PredictiveConfig::disabled(), 80, 24),
            resolver: None,
            keybindings: None,
            journal: None,
        }
    }

    /// Replace the kernel and pane slots with published replicas of
    /// `(id, cols, rows, vt)`.
    pub(super) fn published(mut self, entries: &[(&ResourceId, u16, u16, &[u8])]) -> Self {
        let (kernel, _, panes) = crate::attach::pane_state::published_test_state(entries);
        self.fx.engine_kernel = kernel;
        self.panes = panes;
        self
    }

    /// Install the default config's resolver and keybindings.
    pub(super) fn with_default_bindings(mut self) -> Self {
        let cfg = default_cfg();
        self.resolver =
            Some(phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver"));
        self.keybindings = Some(cfg.keybindings);
        self
    }

    #[allow(clippy::future_not_send, reason = "current-thread test state")]
    pub(super) async fn dispatch(&mut self, mut events: Vec<InputEvent>) -> Sent {
        let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
        let mut conn = Connection::from_stream(a);
        let repainted = {
            let mut ctx = self.fx.ctx();
            ctx.resolver = self.resolver.as_mut();
            ctx.keybindings = self.keybindings.as_ref();
            ctx.input_replay = self.journal;
            dispatch_input_events(
                &mut Vec::new(),
                &mut conn,
                &mut events,
                &mut self.focused,
                &mut self.predict,
                &mut self.panes,
                &mut ctx,
            )
            .await
            .expect("dispatch")
        };
        drop(conn);
        // Per batch, as the driver's flag is per attach: the next starts clear.
        let detach = std::mem::take(&mut self.fx.detach_pending);
        Sent {
            frames: drain(Connection::from_stream(b)).await,
            detach,
            repainted,
        }
    }
}

pub(super) fn default_cfg() -> phux_config::Config {
    phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("default config parses")
}

/// A [`ResolvedAction`] named `name` with `args`.
pub(super) fn act(
    name: &str,
    args: &[(&str, toml::Value)],
) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: name.to_owned(),
        args: args
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    }
}

/// Two leaves split side by side at `ratio`, focus on the left.
pub(super) fn two_pane_workspace_at(ratio: f32) -> ArrangementDoc {
    use crate::layout::{Group, LayoutNode, LayoutState, split_at};
    let tree = split_at(
        &LayoutNode::Leaf(tid(1)),
        &tid(1),
        &tid(2),
        SplitDir::Horizontal,
        ratio,
    )
    .expect("split");
    ArrangementDoc {
        groups: vec![Group::new(
            "1".to_owned(),
            LayoutState {
                tree: Some(tree),
                focus: Some(tid(1)),
            },
        )],
        active: 0,
    }
}

pub(super) fn two_pane_workspace() -> ArrangementDoc {
    two_pane_workspace_at(0.5)
}

pub(super) fn press(key: phux_protocol::input::key::PhysicalKey, text: Option<&str>) -> InputEvent {
    use phux_protocol::input::key::{KeyAction, KeyEvent};
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: text.and_then(|t| t.chars().next()).map(u32::from),
    })
}

pub(super) fn mev(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: ModSet::empty(),
        x,
        y,
    }
}

pub(super) fn mouse(action: MouseAction, button: MouseButton, x: f64, y: f64) -> InputEvent {
    InputEvent::Mouse(mev(action, button, x, y))
}

/// A left-button event at cell `(x, y)`.
pub(super) fn left(action: MouseAction, x: u16, y: u16) -> InputEvent {
    mouse(action, MouseButton::Left, f64::from(x), f64::from(y))
}

/// A right press at cell `(x, y)`.
pub(super) fn right_press(x: u16, y: u16) -> InputEvent {
    mouse(
        MouseAction::Press,
        MouseButton::Right,
        f64::from(x),
        f64::from(y),
    )
}

pub(super) fn targets(needs_you: usize, windows: usize, roster: usize) -> SidebarTargets {
    use crate::render::chrome::sidebar::{SessionRosterTarget, SidebarCounts, SidebarTarget};
    // Window-only fixtures still represent one current session.
    let roster = roster.max(usize::from(windows > 0));
    SidebarTargets {
        counts: SidebarCounts {
            needs_you,
            windows,
            roster,
            active_session: (windows > 0 && roster > 0).then_some(0),
            host_starts: (0..roster.min(128)).fold(0, |mask, j| mask | (1u128 << j)),
            project_starts: 0,
            rule: crate::render::chrome::sidebar::SidebarRule::Trailing,
            plugin: crate::render::chrome::sidebar_sections::PluginShape::default(),
            agents_share: crate::render::chrome::sidebar::DEFAULT_AGENTS_SHARE,
            agent_host_chip: false,
        },
        // Row 0 is local; the rest are peers, so one fixture covers both.
        needs_you: (0..needs_you)
            .map(|j| {
                if j == 0 {
                    SidebarTarget::Window(1)
                } else {
                    SidebarTarget::Session {
                        name: format!("peer-{j}"),
                        id: None,
                        window: Some(2),
                        pane: Some(3),
                        resource: None,
                    }
                }
            })
            .collect(),
        roster: (0..roster)
            .map(|j| {
                Some(SessionRosterTarget {
                    name: format!("space-{j}"),
                    id: None,
                    host: None,
                    switch_host: None,
                })
            })
            .collect(),
        projects: vec![None; roster],
        plugin: Vec::new(),
    }
}
