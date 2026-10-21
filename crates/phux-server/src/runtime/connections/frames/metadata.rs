//! L3 metadata dispatch on an admitted connection.

use super::{FrameDispatch, FrameFlow, FrameKind, FrameReader, direction_invalid};
use crate::runtime::connections::metadata_mutation::{
    handle_delete_metadata, handle_list_metadata, handle_set_metadata,
};
use crate::runtime::connections::metadata_read::handle_get_metadata;
use crate::runtime::connections::metadata_subscriptions::{
    handle_subscribe_events, handle_subscribe_metadata,
};

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// L3 metadata frames (see [`super::is_metadata_frame`]).
    pub(super) async fn metadata(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id, out_tx) = (self.state, self.client_id, &self.plumbing.out_tx);
        match frame {
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_get_metadata(state, client_id, request_id, &scope, &key, out_tx).await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } if crate::runtime::approvals::is_decision(&scope, &key) => {
                crate::runtime::approvals::decide(
                    state, client_id, request_id, &key, &value, out_tx,
                )
                .await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                handle_set_metadata(
                    state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    value,
                    self.root_token,
                );
            }
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_delete_metadata(state, client_id, request_id, &scope, &key);
            }
            FrameKind::ListMetadata { request_id, scope } => {
                handle_list_metadata(state, client_id, request_id, &scope, out_tx).await;
            }
            FrameKind::SubscribeMetadata { scope, key } => {
                handle_subscribe_metadata(state, client_id, scope, key, out_tx);
            }
            FrameKind::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                handle_subscribe_events(state, client_id, terminal, after_seq, out_tx);
            }
            other => return direction_invalid(client_id, &other),
        }
        FrameFlow::Continue(())
    }
}
