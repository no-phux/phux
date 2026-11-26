//! A one-operation local-owner review connection, shared by native bindings.
//! Call on a worker thread. No reconnect, refreshed recipe, or automatic approval.
use crate::{Target, Transport, session::Session};
use phux_protocol::ids::{OperationId, ResourceKey, Revision};
use phux_protocol::native_agent::{
    NativeRequest, NativeRequestPayload, relaunch::RelaunchApproval,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

/// Retains the exact inert host proposal and its original owner connection.
/// Dropping it cancels review without admitting execution.
pub struct RelaunchReview {
    runtime: tokio::runtime::Runtime,
    session: Session,
    approval: RelaunchApproval,
    document: String,
}

impl std::fmt::Debug for RelaunchReview {
    // Recipe, environment and session details stay out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelaunchReview").finish_non_exhaustive()
    }
}

impl RelaunchReview {
    /// Prepare one local-owner review; no execution is admitted.
    ///
    /// # Errors
    /// Remote transports, unavailable/unsupported peers and host refusals fail
    /// closed. This synchronous entry belongs on a native worker, not a UI loop.
    pub fn prepare(
        target: &Target,
        operation: OperationId,
        resource: ResourceKey,
        expected: Revision,
    ) -> Result<Self, String> {
        if !matches!(target.transport, Transport::Uds(_)) {
            return Err(
                "raw archived execution approval is supported only on the local owner Unix socket"
                    .into(),
            );
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let (session, approval) = runtime.block_on(async {
            let mut session = Session::connect_target(
                target,
                "phux-raw-recipe-review".into(),
                crate::session::control_caps(),
            )
            .await
            .map_err(|error| error.to_string())?;
            let request = NativeRequestPayload::new(NativeRequest::PrepareRelaunch {
                operation,
                resource,
                expected,
            })?;
            let (reply, _) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                session.request(1, Command::NativeAgent { request }),
            )
            .await
            .map_err(|_| "raw recipe preparation timed out; nothing was approved".to_owned())?
            .map_err(|error| error.to_string())?
            .into_parts();
            let CommandResult::OkWith(CommandValue::Json(json)) = reply else {
                return Err(format!("raw recipe review refused: {reply:?}"));
            };
            let approval: RelaunchApproval =
                serde_json::from_str(&json).map_err(|error| error.to_string())?;
            if approval.operation != operation
                || approval.resource != resource
                || approval.expected != expected
            {
                return Err("executing host returned a proposal for a different operation".into());
            }
            Ok::<_, String>((session, approval))
        })?;
        let document =
            serde_json::to_string_pretty(&approval).map_err(|error| error.to_string())?;
        Ok(Self {
            runtime,
            session,
            approval,
            document,
        })
    }

    /// The complete escaped JSON document the owner must review, without clipping.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    /// Consume an explicit owner decision for exactly the displayed document.
    /// No boolean, altered document, reconnect or conflict-refresh is accepted.
    ///
    /// # Errors
    /// Changed documents are not sent. Transport failure is an unknown outcome
    /// carrying the original operation, never permission to replay fresh intent.
    pub fn approve(mut self, reviewed_document: &str) -> Result<CommandResult, String> {
        if reviewed_document != self.document {
            return Err("reviewed raw recipe differs from the host proposal".into());
        }
        let operation = self.approval.operation;
        let request = NativeRequestPayload::new(NativeRequest::ApproveRelaunch {
            approval: self.approval,
        })?;
        self.runtime.block_on(async {
            let reply = tokio::time::timeout(std::time::Duration::from_secs(30),
                self.session.request(2, Command::NativeAgent { request })).await
                .map_err(|_| format!("raw relaunch outcome unknown for operation {}: deadline expired; reconcile before retry", operation.to_hex()))?
                .map_err(|error| format!("raw relaunch outcome unknown for operation {}: {error}; reconcile before retry", operation.to_hex()))?.into_parts().0;
            if matches!(&reply, CommandResult::Accepted { operation: acknowledged } if *acknowledged != operation)
                || matches!(&reply, CommandResult::OkWith(CommandValue::Committed(receipt)) if receipt.operation != operation) {
                return Err(format!("raw relaunch outcome unknown for operation {}: mismatched host receipt", operation.to_hex()));
            }
            Ok(reply)
        })
    }
}
