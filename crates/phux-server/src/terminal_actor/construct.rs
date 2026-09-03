//! Constructors and build wiring for [`TerminalActor`]: the public
//! bundle-shaped constructors, the shared `build` path, default-color
//! installation, OSC 10/11 color-query replies, and the libghostty
//! effect handlers.

use super::{
    CancellationToken, CanonicalTerminal, Cell, ColorQueryScanner, CommandBuilder,
    ConsumerAckRequest, ConsumerAttachRequest, ConsumerDetachRequest, ControlRequest,
    DEFAULT_CELL_PX, DEFAULT_INPUT_MAILBOX, DEFAULT_OUTPUT_BROADCAST, DEFAULT_SCROLLBACK,
    EncodedInputRequest, GhosttyTerminal, HashMap, InputEncoderSnapshot, NativeRequestReceivers,
    PerTerminalFocusEncoder, PerTerminalKeyEncoder, PerTerminalMouseEncoder,
    PerTerminalPasteEncoder, PtySource, Rc, RefCell, SizeReportSize, SnapshotSynthesizer,
    SubscribeToEventsRequest, TerminalActor, TerminalActorBundle, TerminalActorError,
    TerminalHandle, TerminalLifecycle, TerminalOptions, UnsubscribeFromEventsRequest, VecDeque,
    adopt_pty, broadcast, color_query_reply, default_shell_command, mpsc, oneshot, osc133,
    resolve_shell, spawn_pty, watch,
};
use phux_config::ScrollbackLimits;

impl TerminalActor {
    /// Build a fresh actor of the given dimensions **without** a backing
    /// PTY. Used by tests that exercise snapshot / shutdown semantics
    /// without driving a real process.
    ///
    /// The `GhosttyTerminal` is allocated via libghostty's default allocator
    /// (NULL alloc → `'static` lifetimes). Scrollback is `DEFAULT_SCROLLBACK`
    /// — a tmux-style mid-range value the runtime overrides with
    /// `defaults.history-limit` / `defaults.history-bytes` via
    /// [`Self::build_with_token`].
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

    /// Build a fresh actor backed by a real PTY running `cmd`.
    ///
    /// Spawns the command on the slave side, kicks off the reader and
    /// writer bridge threads, and returns the bundle. The caller hands
    /// `actor` to `spawn_local` and keeps `handle` + `token` to talk
    /// to and tear down the actor.
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

    /// Convenience: spawn the user's default shell (`$SHELL` or
    /// `/bin/sh`; no server config in scope here) in a fresh PTY.
    pub fn new_with_default_shell(
        cols: u16,
        rows: u16,
    ) -> Result<TerminalActorBundle, TerminalActorError> {
        Self::new_with_command(
            default_shell_command(&resolve_shell(None), false),
            cols,
            rows,
        )
    }

    /// Build an actor whose cancellation token is `token` (typically a
    /// `root_token.child_token()` from [`crate::runtime::ServerRuntime`]).
    /// The bundle's `token` field is a clone of the same token, so
    /// cancelling either propagates to the actor.
    ///
    /// This is the path the runtime uses; tests use [`Self::new`] /
    /// [`Self::new_with_command`] which generate an unlinked fresh
    /// token internally.
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

    /// Runtime constructor that seeds host default colors before the PTY is
    /// spawned and any child output can be parsed.
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

    /// Build an actor around a PTY master fd + child PID inherited across a
    /// graceful-upgrade `execve` (ADR-0032), then replay `seed` (the pane's
    /// snapshot from the [`StateBlob`](crate::upgrade::blob::StateBlob)) into
    /// the fresh `Terminal` so the grid matches what the old image showed.
    ///
    /// The PTY is not re-opened and the child is not re-spawned — both kept
    /// running across the exec; this rebuilds only the server-side plumbing.
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
        let mut terminal = GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            // `defaults.history-limit` is a `u32` on the wire/config; the
            // libghostty option is `usize`. The widen is lossless on all
            // supported targets.
            max_scrollback: scrollback.lines as usize,
        })?;
        // `TerminalOptions::max_scrollback` is only libghostty's *line* limit.
        // The engine enforces a byte limit alongside it and applies whichever
        // is reached first, and a terminal built through the C API keeps
        // Ghostty's 10_000-byte constructor default — floored at two standard
        // pages. That byte floor, not `history-limit`, decided how much
        // history a phux pane kept: 810 rows at 80 columns and 295 rows at
        // 200 columns, whatever `history-limit` said. Install
        // `defaults.history-bytes` explicitly so both bounds are the
        // operator's (ADR-0094).
        terminal.set_scrollback_max_bytes(Some(scrollback.bytes as usize))?;
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
        let (resize_tx, resize_rx) = mpsc::channel(DEFAULT_INPUT_MAILBOX);
        let (consumer_attach_tx, consumer_attach_rx) =
            mpsc::channel::<ConsumerAttachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_detach_tx, consumer_detach_rx) =
            mpsc::channel::<ConsumerDetachRequest>(DEFAULT_INPUT_MAILBOX);
        let (consumer_ack_tx, consumer_ack_rx) =
            mpsc::channel::<ConsumerAckRequest>(DEFAULT_INPUT_MAILBOX);
        let (subscribe_to_events_tx, subscribe_to_events_rx) =
            mpsc::channel::<SubscribeToEventsRequest>(DEFAULT_INPUT_MAILBOX);
        let (unsubscribe_from_events_tx, unsubscribe_from_events_rx) =
            mpsc::channel::<UnsubscribeFromEventsRequest>(DEFAULT_INPUT_MAILBOX);
        let (control_tx, control_rx) = mpsc::channel::<ControlRequest>(DEFAULT_INPUT_MAILBOX);
        let (output_tx, _output_rx_seed) = broadcast::channel(DEFAULT_OUTPUT_BROADCAST);
        let (exit_tx, exit_rx) = oneshot::channel::<Option<i32>>();
        let bundle_token = token.clone();

        let (pty_rx, pty_tx, pty) = match pty_source {
            PtySource::None => (None, None, None),
            PtySource::Spawn(cmd) => {
                let (rx, tx, owned) = spawn_pty(cmd, cols, rows)?;
                (Some(rx), Some(tx), Some(owned))
            }
            PtySource::Adopt {
                master_fd,
                child_pid,
            } => {
                let (rx, tx, owned) = adopt_pty(master_fd, child_pid)?;
                (Some(rx), Some(tx), Some(owned))
            }
        };
        Self::install_effects(&mut terminal, &size_report, pty_tx.as_ref())?;

        let actor = Self {
            terminal: RefCell::new(CanonicalTerminal::Plain(Some(terminal))),
            synth: RefCell::new(synth),
            // A pane may carry initial content (PTY banner, restored
            // scrollback); start dirty so the first tick always emits.
            terminal_dirty_since_tick: true,
            last_input_at: std::cell::Cell::new(None),
            raw_seq: 0,
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
            pending_native_bootstrap: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap_backlog: VecDeque::new(),
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publications: HashMap::new(),
            set_default_colors_rx,
            screen_rx,
            upgrade_rx,
            pwd_rx,
            resize_rx,
            consumer_attach_rx,
            consumer_detach_rx,
            consumer_ack_rx,
            consumer_states: HashMap::new(),
            // phux-yeca: keep the human attach path on the raw PTY
            // broadcast pump by default. The per-consumer synthesized-VT
            // tick path is correct for state-sync experiments, but as the
            // sole emitter it adds a visible 20-30 ms floor to local typing
            // and can lose byte-exact styling that interactive shells/TUIs
            // rely on. Tests can still flip this on explicitly with
            // `enable_tick_emit_for_test`; production needs a negotiated
            // consumer mode before making synthesized ticks the human path.
            consumer_tick_emits: false,
            pty_rx,
            pty_tx,
            pty,
            output_tx: output_tx.clone(),
            exit_notify: Some(exit_tx),
            token,
            event_sink: None,
            last_title: String::new(),
            last_progress: String::new(),
            agent_detect: None,
            agent_state_sink: None,
            agent_dirty_since_detect: false,
            last_ask: None,
            ask_retry_owed: false,
            in_output_burst: false,
            output_since_idle_tick: false,
            event_subscribers: RefCell::new(Vec::new()),
            last_known_cwd: RefCell::new(std::env::var("HOME").unwrap_or_default()),
            osc133: osc133::Osc133Scanner::new(),
            dirty_event_emitted_this_burst: false,
            subscribe_to_events_rx,
            unsubscribe_from_events_rx,
            control_rx,
            lifecycle: TerminalLifecycle::Running,
            wire_terminal_id: 0,
            cols,
            rows,
            cell_px: DEFAULT_CELL_PX,
            size_report,
        };
        let handle = TerminalHandle {
            input: input_tx,
            encoded_input: encoded_input_tx,
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
            upgrade: upgrade_tx,
            pwd: pwd_tx,
            output: output_tx,
            resize: resize_tx,
            consumer_attach: consumer_attach_tx,
            consumer_detach: consumer_detach_tx,
            consumer_ack: consumer_ack_tx,
            subscribe_to_events: subscribe_to_events_tx,
            unsubscribe_from_events: unsubscribe_from_events_tx,
            control: control_tx,
            cols,
            rows,
        };
        Ok(TerminalActorBundle {
            actor,
            handle,
            token: bundle_token,
            exit_notify: Some(exit_rx),
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

    /// Answer OSC 10/11 queries found in a just-parsed PTY chunk from the
    /// canonical terminal's effective colors. The scanner persists across PTY
    /// reads, so an escape sequence split at any byte boundary still works.
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
        let terminal = self.terminal.borrow();
        let foreground = (queries & 1 != 0)
            .then(|| terminal.fg_color().ok().flatten())
            .flatten();
        let background = (queries & 2 != 0)
            .then(|| terminal.bg_color().ok().flatten())
            .flatten();
        drop(terminal);

        let Some(pty_tx) = &self.pty_tx else {
            return;
        };
        if let Some(color) = foreground {
            let _ = pty_tx.try_send(EncodedInputRequest::legacy(color_query_reply(10, color)));
        }
        if let Some(color) = background {
            let _ = pty_tx.try_send(EncodedInputRequest::legacy(color_query_reply(11, color)));
        }
    }

    /// Install the libghostty effect handlers the actor relies on.
    ///
    /// `on_size` answers XTWINOPS size queries (CSI 14/16/18 t) from the
    /// shared geometry cell; `handle_resize` keeps it current. Without
    /// this callback libghostty silently drops the query and pixel-aware
    /// programs (`kitten icat` preflights, sixel sizers) see a mute
    /// terminal even though the kernel winsize carries pixel dims.
    ///
    /// `on_pty_write` routes terminal-generated replies (XTWINOPS size
    /// reports, DECRQM mode reports, CSI 21 t title reports, mode-2048
    /// in-band resize notifications) back to the child through the same
    /// writer bridge that carries client input; libghostty discards
    /// every reply when it is absent. The callback fires synchronously
    /// inside `vt_write` while the actor holds the `Terminal` borrow, so
    /// it must not touch the terminal — a channel send is safe. The
    /// sender it captures is WEAK: the closure lives in the Terminal's
    /// vtable, which the actor owns through `shutdown_pty` — a strong
    /// clone there would keep the writer-bridge channel open while
    /// `shutdown_pty` joins the writer thread (which only exits on
    /// channel close), deadlocking teardown.
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
                // No upgrade ⇒ the writer bridge (and child) are gone;
                // the reply has no recipient. Drop it.
                if let Some(tx) = tx.upgrade() {
                    let _ = tx.try_send(EncodedInputRequest::legacy(bytes.to_vec()));
                }
            })?;
        }
        Ok(())
    }

    /// Test-only constructor: write `bytes` into the actor's `Terminal`
    /// before the actor starts running. Useful for unit and integration
    /// tests that want the snapshot/incremental synthesis path to
    /// return non-trivial content without wiring up a PTY pump.
    ///
    /// Public (rather than `#[cfg(test)]`) so integration tests under
    /// `crates/phux-server/tests/` can call it. Not exercised by
    /// production code; the name + doc make the intent clear.
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
