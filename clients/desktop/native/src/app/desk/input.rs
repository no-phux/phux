//! Text, scrolling, paste and file upload into the focused terminal.

use super::*;

impl Desk {
    pub(super) fn host_action(&mut self, action: HostAction, cx: &mut Context<Self>) {
        if let Some(id) = self.focused_view().and(self.focused_id())
            && let Some(notice) = self.panes.request(&id, action, cx)
        {
            self.post(Kind::Refusal, notice.title, Some(notice.body));
        }
    }

    pub(super) fn send_text(&mut self, text: &str) {
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        if self.refuse_native_input(&terminal) {
            return;
        }
        let events = typed_keys(text);
        let Some(client) = self.client() else { return };
        let sent = client.with_control(|control| {
            if crate::input::native_owned(control, &terminal) {
                return false;
            }
            events
                .into_iter()
                .all(|event| control.send_key(&terminal, event))
        });
        if !sent {
            self.post(
                Kind::Error,
                "Could not send keys",
                Some("The terminal did not accept the input.".into()),
            );
        }
        client.wake();
    }

    pub(super) fn scroll(&mut self, scroll: Scroll, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.scroll_view(view, scroll);
        });
    }

    pub(super) fn scroll_page(&mut self, direction: i64, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let rows = client.acquire_view(view).map_or(1, |frame| frame.rows);
            let _ = client.scroll_view(
                view,
                Scroll::Delta(direction * i64::from(rows.saturating_sub(1)).max(1)),
            );
        });
    }

    pub(super) fn follow(&mut self, cx: &mut Context<Self>) {
        self.with_view(cx, |client, view| {
            let _ = client.follow_live_view(view);
        });
    }

    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.with_engine(cx, |engine, view| {
            let _ = engine.select_all_view(view);
        });
    }

    pub(super) fn paste_selection(&mut self) {
        if let Some(text) = self.selection_text().filter(|text| !text.is_empty()) {
            self.paste_text(&text);
        }
    }

    pub(super) fn paste_cwd(&mut self) {
        if let Some(cwd) = self.focused_cwd() {
            self.paste_text(&crate::attach::quote_paths(&[PathBuf::from(cwd)]));
        }
    }

    /// Fold the focused pane away, then release it. A second close of a pane
    /// already folding does nothing.
    pub(super) fn fold_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.focused_id() else {
            return;
        };
        if !self.motion.leave(&id, Instant::now()) {
            return;
        }
        let span = self.motion.leave_span();
        cx.spawn_in(window, async move |this, cx| {
            smol::Timer::after(span).await;
            let _ = this.update_in(cx, |desk, window, cx| {
                desk.motion.left(&id);
                desk.workspace_call(|link| link.workspace.close_pane(&link.connection, Some(&id)));
                desk.after_command(window, cx);
            });
        })
        .detach();
    }

    /// Paste dropped paths into the pane they were dropped on.
    /// A remote terminal uploads the files and pastes the server path.
    pub fn drop_paths(&mut self, placement_id: &str, paths: &[PathBuf], cx: &mut Context<Self>) {
        self.drop_target = None;
        let Some(terminal) = self
            .placement(placement_id)
            .and_then(|placement| placement.terminal_id.clone())
        else {
            return;
        };
        self.paste_host_paths(&terminal, paths, cx);
    }

    /// Paste paths that live on this Mac, or upload them when the terminal does not.
    pub(super) fn paste_host_paths(
        &mut self,
        terminal: &ResourceId,
        paths: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        if self.refuse_native_input(terminal) {
            return;
        }
        let host = crate::attach::terminal_host(self.remote.as_deref(), terminal);
        match crate::attach::attach_paths(&host, paths) {
            Some(crate::attach::PathAttach::Paste(text)) => {
                self.paste_text_to(terminal, &text);
            }
            Some(crate::attach::PathAttach::Upload(batch)) => {
                self.start_upload(terminal, batch, cx)
            }
            Some(crate::attach::PathAttach::Refuse { title, body }) => {
                self.post(Kind::Refusal, title, Some(body))
            }
            None => {}
        }
    }

    /// A pending or retained native owner never turns back into a raw PTY.
    pub(in crate::app) fn native_owned_terminal(&self, terminal: &ResourceId) -> bool {
        let Some(link) = self.link.as_ref() else {
            return false;
        };
        self.native_launch
            .owns_terminal(&link.connection.client, terminal)
            || link.connection.conversations.values().any(|conversation| {
                conversation.parent == *terminal && conversation.native.is_some()
            })
            || link
                .connection
                .client
                .with_control(|control| crate::input::native_owned(control, terminal))
    }

    pub(super) fn refuse_native_input(&mut self, terminal: &ResourceId) -> bool {
        if !self.native_owned_terminal(terminal) {
            return false;
        }
        self.refuse_paste("Native ACP agents do not accept raw terminal input or file drops. Use the structured conversation. Nothing was sent.");
        true
    }

    pub(super) fn refuse_paste(&mut self, body: impl Into<String>) {
        self.post(
            Kind::Refusal,
            crate::attach::PASTE_REFUSED,
            Some(body.into()),
        );
    }

    pub(super) fn paste_text(&mut self, text: &str) -> bool {
        let Some(terminal) = self.focused_terminal() else {
            return false;
        };
        self.paste_text_to(&terminal, text)
    }

    /// Queue a bracketed paste. False, with a notification, when the terminal is not ready.
    pub(crate) fn paste_text_to(&mut self, terminal: &ResourceId, text: &str) -> bool {
        if self.refuse_native_input(terminal) {
            return false;
        }
        let queued = self.queue_paste(terminal, text);
        if queued == Some(false) {
            self.post(
                Kind::Error,
                "Could not paste",
                Some("The terminal is not ready.".into()),
            );
        }
        queued == Some(true)
    }

    /// Queue a paste when the terminal takes input. `None` when not connected.
    ///
    /// Every caller pastes on the person's own request (Paste Selection,
    /// Insert Path, a dropped file, Send Notes), so it is trusted like
    /// Command-V: the server brackets it for the program and does not refuse
    /// a line break, which an untrusted paste would drop on the floor.
    pub(super) fn queue_paste(&self, terminal: &ResourceId, text: &str) -> Option<bool> {
        let client = self.client()?;
        let queued = client.with_control(|control| {
            if crate::input::native_owned(control, terminal) || !control.input_ready(terminal) {
                return false;
            }
            let _ = control.apply_user_paste(terminal, text);
            true
        });
        if queued {
            client.wake();
        }
        Some(queued)
    }

    /// Read local files off the UI thread, upload them, and paste nothing yet.
    pub(super) fn start_upload(
        &mut self,
        terminal: &ResourceId,
        batch: crate::attach::UploadBatch,
        cx: &mut Context<Self>,
    ) {
        if self.refuse_native_input(terminal) {
            return;
        }
        let Some(client) = self.client() else {
            self.refuse_paste("Not connected. Nothing was pasted.");
            return;
        };
        let Some(started) = self.destination_of(terminal) else {
            self.refuse_paste("That terminal is gone. Nothing was pasted.");
            return;
        };
        if matches!(started.host, crate::attach::TerminalHost::Local) {
            if let crate::attach::UploadBatch::Paths(paths) = batch {
                self.paste_text_to(terminal, &crate::attach::quote_paths(&paths));
            }
            return;
        }
        let epoch = self.upload_epoch;
        self.uploads_reading = self.uploads_reading.saturating_add(1);
        cx.spawn(async move |this, cx| {
            let prepared = smol::unblock(move || crate::attach::prepare_upload(batch)).await;
            let _ = this.update(cx, |desk, cx| {
                desk.uploads_reading = desk.uploads_reading.saturating_sub(1);
                desk.commit_uploads(epoch, &client, &started, prepared);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn commit_uploads(
        &mut self,
        epoch: u64,
        client: &Arc<Client>,
        started: &crate::attach::UploadDestination,
        prepared: Vec<Result<crate::attach::PreparedUpload, crate::attach::UploadPrepareError>>,
    ) {
        // A replaced connection already explained the abort. A late read stops.
        if self.upload_epoch != epoch {
            return;
        }
        if self
            .client()
            .is_none_or(|current| !Arc::ptr_eq(&current, client))
        {
            self.refuse_paste(UPLOAD_MOVED);
            return;
        }
        if self.refuse_native_input(&started.terminal) {
            return;
        }
        let current = self.destination_of(&started.terminal);
        let moved = !crate::attach::same_upload_destination(started, current.as_ref());
        if moved {
            self.refuse_paste(UPLOAD_MOVED);
        }
        for item in prepared {
            match item {
                Err(error) => self.post(Kind::Refusal, error.title, Some(error.body)),
                Ok(_) if moved => {}
                Ok(file) => {
                    let transfer_id =
                        client.put_file(started.terminal.clone(), file.extension, file.bytes);
                    self.uploads.push(PendingUpload {
                        transfer_id,
                        destination: started.clone(),
                        local_source: file.local_source,
                    });
                }
            }
        }
    }

    pub(super) fn finish_uploads(&mut self, receipts: Vec<FileUploadReceipt>) {
        for receipt in receipts {
            let Some(index) = self
                .uploads
                .iter()
                .position(|pending| pending.transfer_id == receipt.transfer_id)
            else {
                continue;
            };
            let pending = self.uploads.remove(index);
            let current = self.destination_of(&pending.destination.terminal);
            match crate::attach::follow_upload(
                &pending.destination,
                current.as_ref(),
                &receipt,
                pending.local_source.as_deref(),
            ) {
                crate::attach::UploadFollowup::Paste { terminal, text } => {
                    let more = self
                        .uploads
                        .iter()
                        .any(|pending| pending.destination.terminal == terminal);
                    let text = if more { format!("{text} ") } else { text };
                    self.paste_uploaded(&terminal, &text);
                }
                crate::attach::UploadFollowup::Notice { title, body } => {
                    self.post(Kind::Refusal, title, Some(body));
                }
            }
        }
    }

    /// Paste a server path. `input_ready` here means the terminal can take
    /// keystrokes, not that its files are on this machine.
    pub(super) fn paste_uploaded(&mut self, terminal: &ResourceId, text: &str) {
        if self.refuse_native_input(terminal) {
            return;
        }
        let outcome = match self.queue_paste(terminal, text) {
            Some(true) => return,
            Some(false) => "The terminal was not ready, so nothing was pasted.",
            None => "Nothing was pasted.",
        };
        self.refuse_paste(format!("The file is on that machine at {text}. {outcome}"));
    }

    /// The placed terminal and the host [`crate::attach::terminal_host`] names.
    /// Absent when that terminal is no longer on screen or belongs to native ACP.
    pub(super) fn destination_of(
        &self,
        terminal: &ResourceId,
    ) -> Option<crate::attach::UploadDestination> {
        if self.native_owned_terminal(terminal) {
            return None;
        }
        self.placement_of(terminal)?;
        Some(crate::attach::UploadDestination {
            terminal: terminal.clone(),
            host: crate::attach::terminal_host(self.remote.as_deref(), terminal),
        })
    }

    pub(super) fn write_file(&mut self, region: &str, mode: &str, cx: &mut Context<Self>) {
        let Some(view) = self.focused_view() else {
            return;
        };
        let Some(client) = self.client() else { return };
        let (text, name, empty) = match region {
            "selection" => {
                let text = self.selection_text().unwrap_or_default();
                (text, "selection.txt", "Nothing selected")
            }
            "scrollback" => {
                let text = region_text(&client, view, TextRegion::History);
                (text, "scrollback.txt", "Nothing to write")
            }
            _ => {
                let text = region_text(&client, view, TextRegion::Screen);
                (text, "screen.txt", "Nothing to write")
            }
        };
        if text.is_empty() {
            self.post(Kind::Refusal, empty, None);
            return;
        }
        let path = match host::write_temp_file(name, &text) {
            Ok(path) => path,
            Err(error) => {
                self.post(Kind::Error, "Could not write the file", Some(error));
                return;
            }
        };
        let shown = path.display().to_string();
        match mode {
            "paste" => {
                let Some(terminal) = self.focused_terminal() else {
                    return;
                };
                self.paste_host_paths(&terminal, &[path], cx);
            }
            "copy" => self.host_action(HostAction::CopyText(shown), cx),
            _ => self.host_action(HostAction::Open(shown), cx),
        }
    }
}
