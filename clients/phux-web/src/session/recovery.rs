//! Recovering a stream the kernel reported stalled or invalid, one resource
//! at a time where the resource allows it.

use super::*;

impl Connection {
    /// Act on the recovery statuses in the last kernel effects, if any:
    /// a stalled document is attached again for a fresh cut, and a lost
    /// one (or a document or agent whose generation was invalidated) loses
    /// only its tile. A failed new split is refused without touching its
    /// siblings; any other invalid Terminal ends the connection.
    pub(super) fn recover_streams(&mut self) -> Option<Outcome> {
        let statuses: Vec<KernelStatus> = self
            .effects
            .as_slice()
            .iter()
            .filter_map(|effect| match effect {
                KernelEffect::Status(
                    status @ (KernelStatus::ResyncRequired { .. }
                    | KernelStatus::DocumentStalled { .. }
                    | KernelStatus::DocumentLost { .. }),
                ) => Some(status.clone()),
                _ => None,
            })
            .collect();
        if statuses.is_empty() {
            return None;
        }
        let split = self
            .pending_split
            .as_ref()
            .and_then(|pending| pending.resource.clone());
        let mut outcome = Outcome {
            render: true,
            ..Outcome::default()
        };
        let mut split_stalled = false;
        for status in statuses {
            match status {
                KernelStatus::DocumentStalled { terminal_id, .. } => {
                    let request_id = self.next_pane_request();
                    outcome.send.push(encode(&FrameKind::Command {
                        request_id,
                        command: Command::attach_terminal(terminal_id, None),
                    }));
                }
                KernelStatus::DocumentLost { terminal_id, .. } => self.lose_document(&terminal_id),
                KernelStatus::ResyncRequired { terminal_id, .. }
                    if self.is_document(&terminal_id) || self.is_agent_session(&terminal_id) =>
                {
                    self.lose_document(&terminal_id);
                }
                KernelStatus::ResyncRequired { terminal_id, .. }
                    if Some(&terminal_id) == split.as_ref() =>
                {
                    split_stalled = true;
                }
                _ => return Some(self.protocol_failure("bootstrap staging timed out")),
            }
        }
        if split_stalled {
            let refused = self.pane_refusal(
                "New pane bootstrap timed out. The other panes are still available; try splitting again."
                    .to_owned(),
            );
            outcome.send.extend(refused.send);
            outcome.panes = true;
        }
        Some(outcome)
    }
}
