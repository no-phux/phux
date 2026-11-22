//! Server frames: the one entry point and the handshake-to-history reducer.

use super::*;

impl Connection {
    /// Reduce one decoded server frame through the shared kernel.
    ///
    /// A fault on an `AgentSession` or document stream (the kernel retires
    /// that generation and asks for a resync) never fails the connection: the
    /// browser cannot reopen the stream, and a badge or a tile is not worth
    /// the terminal.
    pub fn on_frame(&mut self, frame: FrameKind) -> Outcome {
        if self.failed {
            return Outcome::default();
        }
        if let Some(outcome) = self.approvals.on_frame(&frame) {
            return outcome;
        }
        if let FrameKind::PathResults { request_id, result } = frame {
            self.accept_path_results(request_id, result);
            return Outcome::default();
        }
        let stream = KernelInput::from_stream_frame(&frame);
        let resource = match &frame {
            FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id),
            _ => stream.as_ref().and_then(KernelInput::resource_id),
        };
        if resource.is_some_and(|id| self.retiring_panes.contains(id))
            && !matches!(frame, FrameKind::ResourceClosed { .. })
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
        if let FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } = &frame
            && self.accept_path_error(*request_id, message)
        {
            return Outcome::default();
        }
        // A document's fault likewise costs only its tile, which says so.
        let optional = resource
            .filter(|id| self.is_agent_session(id) || self.is_document(id))
            .cloned();
        let split_frame = resource.is_some_and(|id| {
            self.pending_split
                .as_ref()
                .and_then(|pending| pending.resource.as_ref())
                == Some(id)
        });
        let mut outcome = match stream {
            Some(input) => self.reduce_stream(input),
            None => self.reduce_frame(frame),
        };
        if let Some(id) = &optional
            && outcome.fatal.take().is_some()
        {
            self.failed = false;
            self.lose_document(id);
            outcome.render = true;
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
        if let FrameKind::Error {
            request_id: None,
            code: ErrorCode::VersionIncompatible,
            message,
        } = &frame
        {
            return self.version_refusal(message);
        }
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
                server_caps,
                ..
            } => {
                if self.kernel.is_some() {
                    return self.protocol_failure("server sent duplicate HELLO_OK");
                }
                // This client offers Terminal profiles, so a server that
                // selects none broke the offer.
                let Some(selected_profile) = selected_profile else {
                    return self.protocol_failure(&HelloOkError::NoProfile.to_string());
                };
                if let Err(error) = validate_hello_ok(
                    &self.offered_caps,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    selected_profile,
                    bootstrap_limits,
                ) {
                    if matches!(error, HelloOkError::UnsupportedProtocol { .. }) {
                        return self.version_refusal(&error.to_string());
                    }
                    return self.protocol_failure(&error.to_string());
                }
                self.server_protocol = Some(Version {
                    major: protocol_major,
                    minor: protocol_minor,
                    patch: protocol_patch,
                });
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
                let mut send = vec![subscribe, self.discovery_request()];
                send.extend(
                    self.approvals
                        .negotiate(server_caps.features.contains(ServerFeature::Approvals)),
                );
                Outcome {
                    // Subscribe before taking the attach snapshot: a child
                    // created during bootstrap must not fall into a discovery gap.
                    // The kernel's post-ready subscription is idempotent. The
                    // discovery reply chooses the session and sends the ATTACH.
                    send,
                    approvals: true,
                    ..Outcome::default()
                }
            }
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => self.accept_snapshot(attach_id, snapshot),
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

    fn reduce_stream(&mut self, input: KernelInput<'_>) -> Outcome {
        let initial_focus = match &input {
            KernelInput::BootstrapBegin {
                terminal_id,
                profile,
                ..
            } if !matches!(profile, BootstrapStreamProfile::Unknown { .. }) => Some(*terminal_id),
            _ => None,
        };
        let (outcome, applied) = self.apply_kernel(input);
        if applied
            && let Some(terminal_id) = initial_focus
            && !self.is_agent_session(terminal_id)
            && !self.is_document(terminal_id)
        {
            self.focused_terminal
                .get_or_insert_with(|| terminal_id.clone());
        }
        outcome
    }

    /// Fail on a protocol-major mismatch, marked so the page stops redialing.
    fn version_refusal(&mut self, detail: &str) -> Outcome {
        self.version_refused = true;
        let message = format!(
            "{VERSION_REFUSAL}: this page speaks phux protocol {}.x and the server does not \
             ({detail}); update the server or this client, then reload",
            PROTOCOL_VERSION.major
        );
        self.protocol_failure(&message)
    }
}
