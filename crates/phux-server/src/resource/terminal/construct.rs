//! Constructors and build wiring for [`TerminalActor`], default colors,
//! OSC 10/11 replies, and libghostty effect handlers.

use super::process_facet::ChildFacts;
use super::{
    CancellationToken, CanonicalTerminal, Cell, ColorQueryScanner, CommandBuilder,
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, DEFAULT_CELL_PX,
    DEFAULT_INPUT_MAILBOX, DEFAULT_SCROLLBACK, EncodedInputRequest, GhosttyTerminal, HashMap,
    InputEncoderSnapshot, NativeRequestReceivers, PerTerminalFocusEncoder, PerTerminalKeyEncoder,
    PerTerminalMouseEncoder, PerTerminalPasteEncoder, PtyEvent, PtyOwned, PtySource, Rc, RefCell,
    ResourceCore, ResourceFacetHandle, ResourceHandle, ResourceKind, ResourceLifecycle,
    SizeReportSize, SnapshotSynthesizer, TerminalActor, TerminalActorBundle, TerminalActorError,
    TerminalHandle, VecDeque, adopt_pty, color_query_reply, mpsc, osc133, spawn_pty, watch,
};
use phux_config::ScrollbackLimits;

impl TerminalActor {
    /// A PTY-less actor of the given size (tests). Scrollback is
    /// `DEFAULT_SCROLLBACK`; the runtime uses [`Self::build_with_token`].
    #[allow(clippy::new_ret_no_self, reason = "bundle-shaped constructor")]
    pub fn new(cols: u16, rows: u16) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            PtySource::None,
            DEFAULT_SCROLLBACK,
            CancellationToken::new(),
            None,
        )
    }

    /// An actor backed by a real PTY running `cmd`. Hand `actor` to
    /// `spawn_local`; keep `handle` and `token`.
    pub fn new_with_command(
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            PtySource::Spawn(cmd),
            DEFAULT_SCROLLBACK,
            CancellationToken::new(),
            None,
        )
    }

    /// Build an actor cancelled by `token` (usually a runtime child token);
    /// the bundle's `token` is a clone of it. The runtime path.
    pub fn build_with_token(
        cols: u16,
        rows: u16,
        cmd: Option<CommandBuilder>,
        scrollback: ScrollbackLimits,
        token: CancellationToken,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            cmd.map_or(PtySource::None, PtySource::Spawn),
            scrollback,
            token,
            None,
        )
    }

    /// Like [`Self::build_with_token`], seeding host default colors before
    /// any child output is parsed.
    pub fn build_with_token_and_colors(
        cols: u16,
        rows: u16,
        cmd: Option<CommandBuilder>,
        scrollback: ScrollbackLimits,
        token: CancellationToken,
        default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::build(
            cols,
            rows,
            cmd.map_or(PtySource::None, PtySource::Spawn),
            scrollback,
            token,
            default_colors,
        )
    }

    /// Rebuild an actor around a PTY master fd and child pid inherited across
    /// a graceful-upgrade exec (ADR-0032), replaying `seed` so the grid
    /// matches the old image.
    pub fn new_with_adopted_pty(
        master_fd: std::os::fd::RawFd,
        child_pid: i32,
        cols: u16,
        rows: u16,
        scrollback: ScrollbackLimits,
        token: CancellationToken,
        seed: &[u8],
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let bundle = Self::build(
            cols,
            rows,
            PtySource::Adopt {
                master_fd,
                child_pid,
            },
            scrollback,
            token,
            None,
        )?;
        bundle.actor.terminal.borrow_mut().vt_write(seed);
        bundle.actor.publish_input_snapshot();
        Ok(bundle)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "straight-line wiring: one channel pair per request mailbox, then a single actor + handle struct literal. Splitting on an arbitrary boundary separates a channel's two halves from where the actor/handle consume them, which is harder to follow than the linear form."
    )]
    pub(super) fn build(
        cols: u16,
        rows: u16,
        pty_source: PtySource,
        scrollback: ScrollbackLimits,
        token: CancellationToken,
        default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let mut terminal = {
            let mut terminal = GhosttyTerminal::new(cols, rows)?;
            terminal.set_scrollback_max_lines(Some(scrollback.lines as usize))?;
            terminal
        };
        // Install the byte limit too: libghostty applies whichever bound is
        // hit first, and its constructor default would otherwise decide
        // (ADR-0094).
        terminal.set_scrollback_max_bytes(Some(scrollback.bytes as usize))?;
        terminal.set_continuation_max_bytes(64 * 1024 * 1024)?;
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal)?;
        if let Some(colors) = default_colors {
            Self::install_default_colors(&mut terminal, colors)?;
        }
        let size_report = Rc::new(Cell::new(SizeReportSize {
            rows,
            columns: cols,
            cell_width: u32::from(DEFAULT_CELL_PX.0),
            cell_height: u32::from(DEFAULT_CELL_PX.1),
        }));
        let synth = SnapshotSynthesizer::new()?;
        let key_enc = PerTerminalKeyEncoder::new()?;
        let mouse_enc = PerTerminalMouseEncoder::new()?;
        let initial_input_snapshot = InputEncoderSnapshot::capture(&terminal, DEFAULT_CELL_PX)?;
        let (input_tx, input_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (encoded_input_tx, encoded_input_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (input_snapshot_tx, input_snapshot_rx) = watch::channel(initial_input_snapshot);
        let (snapshot_tx, snapshot_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_bootstrap_tx, native_bootstrap_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_publication_tx, native_publication_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_history_tx, native_history_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_release_tx, native_release_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let native_requests = NativeRequestReceivers {
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            bootstrap: native_bootstrap_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            publication: native_publication_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            history: native_history_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            release: native_release_rx,
        };
        let (set_default_colors_tx, set_default_colors_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (screen_tx, screen_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (upgrade_tx, upgrade_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (pwd_tx, pwd_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (process_tx, process_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (resize_tx, resize_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (consumer_attach_tx, consumer_attach_rx) =
            mpsc::channel::<ConsumerAttachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_detach_tx, consumer_detach_rx) =
            mpsc::channel::<ConsumerDetachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_ack_tx, consumer_ack_rx) =
            mpsc::channel::<ConsumerAckRequest>(DEFAULT_INPUT_MAILBOX);
        let bundle_token = token.clone();
        // A Terminal is a root resource: it has no parent in this program.
        let (core, core_channels) = ResourceCore::new(
            ResourceKind::Terminal,
            None,
            token,
            crate::resource::output_broadcast_capacity(),
        );

        let (pty_rx, pty_tx, pty) = initialize_pty(pty_source, cols, rows)?;
        let child_facts = ChildFacts::capture(pty.as_ref());
        Self::install_effects(&mut terminal, &size_report, pty_tx.as_ref())?;

        let actor = Self {
            terminal: RefCell::new(CanonicalTerminal::Plain(Some(terminal))),
            synth: RefCell::new(synth),
            // Initial content may exist; start dirty so the first tick emits.
            terminal_dirty_since_tick: true,
            last_input_at: std::cell::Cell::new(None),
            last_output_at: std::cell::Cell::new(None),
            core,
            color_query_scanner: ColorQueryScanner::default(),
            key_enc: RefCell::new(key_enc),
            mouse_enc: RefCell::new(mouse_enc),
            focus_enc: RefCell::new(PerTerminalFocusEncoder::new()),
            paste_enc: RefCell::new(PerTerminalPasteEncoder::new()),
            input_rx,
            encoded_input_rx,
            input_snapshot_tx,
            snapshot_rx,
            native_requests,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_cursor_owners: HashMap::new(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            reflow_tombstoned: Vec::new(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            pending_native_bootstrap: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap_backlog: VecDeque::new(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            pending_native_history: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history_backlog: VecDeque::new(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publications: HashMap::new(),
            set_default_colors_rx,
            screen_rx,
            upgrade_rx,
            pwd_rx,
            process_rx,
            resize_rx,
            consumer_attach_rx,
            consumer_detach_rx,
            consumer_ack_rx,
            consumer_states: HashMap::new(),
            // Human attach stays on raw PTY bytes; synthesized ticks add
            // latency and lose byte-exact styling. Tests opt in.
            consumer_tick_emits: false,
            pty_rx,
            pty_burst: Vec::new(),
            pty_tx,
            pty,
            event_sink: None,
            last_title: String::new(),
            last_progress: String::new(),
            agent_detect: None,
            agent_state_sink: None,
            live_session_probe: None,
            agent_session_append: None,
            agent_dirty_since_detect: false,
            last_ask: None,
            ask_retry_owed: false,
            in_output_burst: false,
            output_since_idle_tick: false,
            // The child's real starting directory, not `$HOME`.
            last_known_cwd: RefCell::new(child_facts.cwd),
            cwd_announced: Cell::new(false),
            osc133: osc133::Osc133Scanner::new(),
            prompt: osc133::PromptTracker::default(),
            child_start_ms: child_facts.start_ms,
            released_child_pid: None,
            exit: None,
            lifecycle: ResourceLifecycle::Running,
            cols,
            rows,
            cell_px: DEFAULT_CELL_PX,
            size_report,
        };
        let facet = TerminalHandle {
            input: input_tx,
            encoded_input: encoded_input_tx,
            input_credits: super::InputCreditPool::default(),
            input_snapshot: input_snapshot_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: native_bootstrap_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication: native_publication_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history: native_history_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_release: native_release_tx,
            snapshot: snapshot_tx,
            set_default_colors: set_default_colors_tx,
            screen: screen_tx,
            pwd: pwd_tx,
            process: process_tx,
            resize: resize_tx,
            cols,
            rows,
        };
        let handle = ResourceHandle {
            kind: actor.core.kind(),
            parent: actor.core.parent(),
            output: core_channels.output,
            consumer_attach: consumer_attach_tx,
            consumer_detach: consumer_detach_tx,
            consumer_ack: consumer_ack_tx,
            upgrade: upgrade_tx,
            control: core_channels.control,
            facet: ResourceFacetHandle::Terminal(facet),
        };
        Ok(TerminalActorBundle {
            actor,
            handle,
            token: bundle_token,
            exit_notify: Some(core_channels.exit_notify),
        })
    }

    pub(super) fn install_default_colors(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        colors: phux_protocol::caps::TerminalDefaultColors,
    ) -> Result<(), TerminalActorError> {
        use libghostty_vt::style::RgbColor;

        terminal.set_default_fg_color(Some(RgbColor {
            r: colors.foreground.r,
            g: colors.foreground.g,
            b: colors.foreground.b,
        }))?;
        terminal.set_default_bg_color(Some(RgbColor {
            r: colors.background.r,
            g: colors.background.g,
            b: colors.background.b,
        }))?;
        Ok(())
    }

    /// Answer OSC 10/11 queries in a parsed chunk from the canonical colors.
    /// The scanner persists, so split sequences still work.
    pub(super) fn answer_color_queries(&mut self, bytes: &[u8]) {
        let mut queries = 0_u8;
        self.color_query_scanner.feed(bytes, |selector| {
            queries |= match selector {
                10 => 1,
                11 => 2,
                _ => 0,
            };
        });
        if queries == 0 {
            return;
        }
        let canonical = self.terminal.borrow();
        let Some(terminal) = canonical.try_terminal() else {
            // No palette to answer from; a wrong color is worse than none.
            tracing::trace!("colour query unanswered: canonical terminal is on loan");
            return;
        };
        let foreground = (queries & 1 != 0)
            .then(|| terminal.fg_color().ok().flatten())
            .flatten();
        let background = (queries & 2 != 0)
            .then(|| terminal.bg_color().ok().flatten())
            .flatten();
        drop(canonical);

        let Some(pty_tx) = &self.pty_tx else {
            return;
        };
        if let Some(color) = foreground {
            let _ = super::try_send_to_writer(
                pty_tx,
                EncodedInputRequest::legacy(color_query_reply(10, color)),
            );
        }
        if let Some(color) = background {
            let _ = super::try_send_to_writer(
                pty_tx,
                EncodedInputRequest::legacy(color_query_reply(11, color)),
            );
        }
    }

    /// Install libghostty's effect handlers.
    ///
    /// `on_size` answers XTWINOPS size queries from the shared geometry.
    /// `on_pty_write` routes terminal-generated replies back to the child via
    /// the writer bridge. It fires inside `vt_write`, so it only sends on a
    /// channel, and holds the sender weakly: a strong clone would keep the
    /// writer channel open and deadlock `shutdown_pty`'s join.
    pub(super) fn install_effects(
        terminal: &mut GhosttyTerminal<'static, 'static>,
        size_report: &Rc<Cell<SizeReportSize>>,
        pty_tx: Option<&mpsc::Sender<EncodedInputRequest>>,
    ) -> Result<(), TerminalActorError> {
        terminal.on_size({
            let size_report = Rc::clone(size_report);
            move |_term| Some(size_report.get())
        })?;
        if let Some(tx) = pty_tx {
            let tx = tx.downgrade();
            terminal.on_pty_write(move |_term, bytes| {
                // Writer gone: nobody to reply to.
                if let Some(tx) = tx.upgrade() {
                    let _ =
                        super::try_send_to_writer(&tx, EncodedInputRequest::legacy(bytes.to_vec()));
                }
            })?;
        }
        Ok(())
    }

    /// Write `bytes` into the terminal before the actor runs (tests,
    /// including integration tests).
    pub fn new_with_seed(
        cols: u16,
        rows: u16,
        bytes: &[u8],
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        let bundle = Self::new(cols, rows)?;
        bundle.actor.terminal.borrow_mut().vt_write(bytes);
        bundle.actor.publish_input_snapshot();
        Ok(bundle)
    }
}

type OptionalPty = (
    Option<mpsc::Receiver<PtyEvent>>,
    Option<mpsc::Sender<EncodedInputRequest>>,
    Option<PtyOwned>,
);

fn initialize_pty(
    source: PtySource,
    cols: u16,
    rows: u16,
) -> Result<OptionalPty, TerminalActorError> {
    let opened = match source {
        PtySource::None => return Ok((None, None, None)),
        PtySource::Spawn(command) => spawn_pty(command, cols, rows)?,
        PtySource::Adopt {
            master_fd,
            child_pid,
        } => adopt_pty(master_fd, child_pid)?,
    };
    Ok((Some(opened.0), Some(opened.1), Some(opened.2)))
}
