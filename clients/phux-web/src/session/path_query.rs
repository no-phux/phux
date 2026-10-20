//! Host-side path discovery for the focused pane.

use super::*;

impl Connection {
    /// Whether this peer explicitly offers host-side path discovery.
    #[must_use]
    pub const fn path_query_supported(&self) -> bool {
        self.path_query_supported
    }

    /// Results for the latest query, if it completed for the current pane.
    #[must_use]
    pub fn path_results(&self) -> Option<&PathResults> {
        self.path_results.as_ref()
    }

    /// Display-only refusal for the latest query.
    #[must_use]
    pub fn path_error(&self) -> Option<&str> {
        self.path_error.as_deref()
    }

    /// True while awaiting a reply from the selected host.
    #[must_use]
    pub const fn path_pending(&self) -> bool {
        self.path_pending.is_some()
    }

    /// Cancel the UI's interest in the outstanding request. The server may still reply.
    pub fn cancel_path_query(&mut self) {
        self.path_pending = None;
        self.path_target = None;
        self.path_results = None;
        self.path_error = None;
    }

    /// Start one host query, replacing any prior query. No browser filesystem is read.
    #[must_use]
    pub fn path_query_frame(
        &mut self,
        root: &str,
        query: &str,
        recursive: bool,
    ) -> Option<Vec<u8>> {
        if !self.path_query_supported || !self.attach_ready || self.failed {
            return None;
        }
        let target = self.first_published_terminal()?;
        let host = target.host().cloned();
        self.path_request_id = self.path_request_id.wrapping_add(1).max(1);
        self.path_pending = Some(self.path_request_id);
        self.path_target = Some(target);
        self.path_results = None;
        self.path_error = None;
        Some(encode(&FrameKind::PathQuery {
            request_id: self.path_request_id,
            root: root.to_owned(),
            query: query.to_owned(),
            recursive,
            host,
        }))
    }

    /// Insert a selected, original server path as editable POSIX shell text.
    /// The kernel checks the pane's current input lease before emitting a paste.
    #[must_use]
    pub fn paste_path_row(&mut self, index: usize) -> Option<Vec<u8>> {
        let target = self.eligible_path_target()?;
        let path = &self.path_results.as_ref()?.rows.get(index)?.path;
        let text = shell_quote_path(path)?;
        let (outcome, applied) = self.apply_kernel(KernelInput::Action(KernelAction::Input {
            terminal_id: &target,
            event: &InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Untrusted,
                data: text.into_bytes(),
            }),
        }));
        if !applied {
            return None;
        }
        let frame = outcome.send.into_iter().next();
        if frame.is_some() {
            self.cancel_path_query();
        }
        frame
    }

    fn eligible_path_target(&self) -> Option<ResourceId> {
        let target = self.path_target.as_ref()?;
        if self.path_pending.is_some() || self.first_published_terminal().as_ref() != Some(target) {
            return None;
        }
        let kernel = self.kernel.as_ref()?;
        matches!(
            kernel.input_eligibility(target),
            InputEligibility::Eligible { .. }
        )
        .then(|| target.clone())
    }

    pub(super) fn accept_path_results(&mut self, request_id: u32, result: PathQueryResult) {
        if !self.path_query_supported || self.path_pending != Some(request_id) {
            return;
        }
        if self.path_target.as_ref() != self.first_published_terminal().as_ref() {
            self.cancel_path_query();
            return;
        }
        self.path_pending = None;
        match result {
            Ok(results) => self.path_results = Some(results),
            Err(error) => self.path_error = Some(error.message),
        }
    }
}
