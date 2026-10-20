//! Server frames: the one entry point and the handshake-to-history reducer.

use super::*;

impl Connection {
    /// Reduce one decoded server frame through the shared kernel.
    ///
    /// A fault on an `AgentSession` stream (the kernel retires that generation
    /// and asks for a resync) never fails the connection: the browser cannot
    /// reopen an agent stream, and a badge is not worth the terminal.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        if self.failed {
            return Outcome::default();
        }
        if frame_resource_id(&frame).is_some_and(|id| self.retiring_panes.contains(id))
            && !matches!(&frame, FrameKind::ResourceClosed { .. })
        {
            return Outcome::default();
        }
        if let FrameKind::HelloOk { server_caps, .. } = &frame {
            self.path_query_supported = server_caps
                .features_ext
                .contains(ServerFeatureExt::PathQuery);
            self.spawn_initial_size = server_caps
                .features
                .contains(ServerFeature::SpawnInitialSize);
            self.organization = server_caps
                .features_ext
                .contains(ServerFeatureExt::Organization);
        }
        if let FrameKind::PathResults { request_id, result } = frame {
            self.accept_path_results(request_id, result);
            return Outcome::default();
        }
        let agent_frame = frame_resource_id(&frame).is_some_and(|id| self.is_agent_session(id));
        let split_frame = frame_resource_id(&frame).is_some_and(|id| {
            self.pending_split
                .as_ref()
                .and_then(|pending| pending.resource.as_ref())
                == Some(id)
        });
        let mut outcome = self.reduce_frame(frame);
        if agent_frame && outcome.fatal.is_some() {
            self.failed = false;
            outcome.fatal = None;
        }
        if split_frame && let Some(message) = outcome.fatal.take() {
            self.failed = false;
            return self.pane_refusal(format!(
                "New pane bootstrap failed: {message}. The other panes are still available."
            ));
        }
        self.finish_split(&mut outcome);
        outcome
    }

    fn reduce_frame(&mut self, frame: FrameKind) -> Outcome {
        if let Some(outcome) = self.reduce_discovery_reply(&frame) {
            return outcome;
        }
        if let Some(outcome) = self.reduce_agent_reply(&frame) {
            return outcome;
        }
        if let Some(outcome) = self.reduce_pane_frame(&frame) {
            return outcome;
        }
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                selected_profile,
                bootstrap_limits,
                ..
            } => {
                if self.kernel.is_some() {
                    return self.protocol_failure("server sent duplicate HELLO_OK");
                }
                if let Err(message) = validate_hello_ok(
                    self.offered_caps,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                ) {
                    return self.protocol_failure(message);
                }
                self.bootstrap_limits = Some(bootstrap_limits);
                self.selected_profile = Some(selected_profile);
                let history_config = HistoryCacheConfig {
                    request_max_bytes: bootstrap_limits.max_history_page_bytes(),
                    ..HistoryCacheConfig::default()
                };
                self.kernel = Some(SessionKernel::with_history_config(
                    WebEngine {
                        vt: Rc::clone(&self.vt),
                        limits: bootstrap_limits,
                    },
                    selected_profile,
                    history_config,
                ));
                let subscribe = encode(&FrameKind::SubscribeEvents {
                    terminal: None,
                    after_seq: None,
                });
                Outcome {
                    // Subscribe before taking the attach snapshot: a child
                    // created during bootstrap must not fall into a discovery gap.
                    // The kernel's post-ready subscription is idempotent. The
                    // discovery reply chooses the session and sends the ATTACH.
                    send: vec![subscribe, self.discovery_request()],
                    ..Outcome::default()
                }
            }
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => self.accept_snapshot(attach_id, snapshot),
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                let (outcome, applied) = self.apply_kernel(KernelInput::BootstrapBegin {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    profile,
                    geometry: CanonicalGeometry { cols, rows },
                    base_seq,
                });
                if applied && !self.is_agent_session(&terminal_id) {
                    self.focused_terminal
                        .get_or_insert_with(|| terminal_id.clone());
                }
                outcome
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                self.apply_kernel(KernelInput::BootstrapChunk {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    chunk_seq,
                    payload: &payload,
                })
                .0
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                self.apply_kernel(KernelInput::BootstrapReady {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    history_cursor: history_cursor.as_deref(),
                })
                .0
            }
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                cursor,
                next_cursor,
                payload,
            } => {
                self.apply_kernel(KernelInput::HistoryPage {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    page_seq,
                    rows,
                    payload: &payload,
                    cursor: &cursor,
                    next_cursor: next_cursor.as_deref(),
                })
                .0
            }
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
                let Some(reason) = history_unavailable_reason(reason) else {
                    return self.protocol_failure("unsupported history tombstone reason");
                };
                self.apply_kernel(KernelInput::HistoryTombstone {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: &cursor,
                    reason,
                })
                .0
            }
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                let Some(reason) = history_rejection_reason(reason) else {
                    return self.protocol_failure("unsupported history rejection reason");
                };
                self.apply_kernel(KernelInput::HistoryRejected {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor: &cursor,
                    reason,
                    required_bytes,
                    required_rows,
                })
                .0
            }
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                self.apply_kernel(KernelInput::ResourceOutput {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                    payload: &bytes,
                })
                .0
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                self.apply_kernel(KernelInput::Tombstone {
                    terminal_id: &terminal_id,
                    stream_id,
                    bootstrap_id,
                    reason,
                    last_valid_seq,
                })
                .0
            }
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => self.close_resource(terminal_id, exit_status, reason, signal),
            FrameKind::AttachReady { attach_id } => self.finish_attach(attach_id),
            FrameKind::Event {
                terminal: Some(id),
                event,
                ..
            } => self.reduce_resource_event(&id, &event),
            _ => Outcome::default(),
        }
    }
}
