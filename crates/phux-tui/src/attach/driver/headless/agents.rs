//! Explicit agent streams are part of a settled snapshot, not aggregate ATTACH.
use super::{
    AttachError, Connection, FrameKind, HeadlessSession, RESOURCE_AGENT_KEY, ResourceId, Scope,
};

impl HeadlessSession {
    const fn take_agent_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }

    /// Read each pane's metadata and every admitted agent's retained stream.
    #[allow(
        clippy::future_not_send,
        reason = "native terminal owner stays on its current thread"
    )]
    pub(super) async fn request_agent_records(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let panes = self.mirror.panes.keys().cloned().collect::<Vec<_>>();
        for id in panes {
            let request_id = self.take_agent_request_id();
            self.mirror
                .agent_meta
                .pending
                .insert(request_id, id.clone());
            conn.send(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Resource(id),
                key: RESOURCE_AGENT_KEY.to_owned(),
            })
            .await?;
        }
        let agents = self
            .mirror
            .engine_kernel
            .agent_sessions()
            .map(|agent| agent.terminal_id.clone())
            .collect::<Vec<_>>();
        self.request_agent_streams(conn, &agents).await
    }

    #[allow(
        clippy::future_not_send,
        reason = "native terminal owner stays on its current thread"
    )]
    pub(super) async fn request_agent_streams(
        &mut self,
        conn: &mut Connection,
        ids: &[ResourceId],
    ) -> Result<(), AttachError> {
        for id in ids {
            let parent = self
                .mirror
                .engine_kernel
                .agent_session(id)
                .and_then(|agent| agent.parent);
            if !parent
                .is_some_and(|parent| self.mirror.engine_kernel.active_attach_contains(parent))
                || self
                    .mirror
                    .pending_resource_ops
                    .values()
                    .any(|pending| pending == id)
            {
                continue;
            }
            let request_id = self.take_agent_request_id();
            let after_seq = conn
                .server_features_ext()
                .contains(phux_protocol::ServerFeatureExt::AgentStreamResume)
                .then(|| self.mirror.engine_kernel.agent_stream_info(id))
                .flatten()
                .map(|stream| stream.last_seq);
            self.mirror
                .pending_resource_ops
                .insert(request_id, id.clone());
            conn.send(&FrameKind::Command {
                request_id,
                command: phux_protocol::wire::frame::Command::attach_agent_session(
                    id.clone(),
                    conn.observer_role_policy(),
                    after_seq,
                ),
            })
            .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
