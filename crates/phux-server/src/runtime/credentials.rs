//! Mandatory per-request approval and one-use delivery, on any authenticated transport.

use super::commands::CommandContext;
use crate::state::{Decision, OpenedApproval, SharedState};
use phux_protocol::credentials::{CredentialOp, CredentialRequest, SecretValues};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{
    ApprovalOutcome, Command, CommandResult, CommandValue, ErrorCode,
};
use serde_json::json;
use std::time::Duration;

pub(super) async fn handle(ctx: &CommandContext, op: CredentialOp) -> CommandResult {
    if op.validate().is_err() {
        return invalid("invalid credential operation");
    }
    match op {
        CredentialOp::Request(request) => request_values(ctx, request).await,
        CredentialOp::Next { provider } => next(ctx, &provider),
        CredentialOp::Complete { id, values } => complete(ctx, &id, values),
    }
}

fn next(ctx: &CommandContext, provider: &str) -> CommandResult {
    ctx.state.with_mut(|s| {
        if let Err(message) = s.credentials.register(provider, ctx.client_id) { return invalid(message); }
        let Some((id, request, requester)) = s.credentials.claim(ctx.client_id, provider) else { return CommandResult::Ok; };
        let actor = s.client_actor_ref(requester);
        CommandResult::OkWith(CommandValue::Json(json!({
            "id": id, "request": request, "requester": {
                "client": actor.client.get(), "credential_id": actor.credential_id, "client_name": actor.client_name,
            },
        }).to_string()))
    })
}

fn complete(ctx: &CommandContext, id: &str, values: Option<SecretValues>) -> CommandResult {
    ctx.state.with_mut(|s| {
        let Some((requester, request)) = s.credentials.completion_request(id, ctx.client_id) else {
            return denied("no claimed credential request for this provider connection");
        };
        if values
            .as_ref()
            .is_some_and(|v| v.0.len() != request.references.len())
        {
            return invalid("credential reply count does not match the approved references");
        }
        let command = Command::Credentials {
            op: CredentialOp::Request(request),
        };
        if crate::policy::authorize_command(s, requester, &command).is_err() {
            s.credentials.complete(id, None);
            return denied("credential requester is no longer authorized");
        }
        s.credentials.complete(id, values);
        CommandResult::Ok
    })
}

fn open_request(
    ctx: &CommandContext,
    request: CredentialRequest,
    command: &Command,
) -> Result<
    (
        OpenedApproval,
        tokio::sync::oneshot::Receiver<Option<SecretValues>>,
    ),
    CommandResult,
> {
    ctx.state.with_mut(|s| {
        let provider = s.credentials.provider(&request.provider).ok_or_else(|| {
            invalid(
                "1Password provider is not connected; start phux credentials serve on that device",
            )
        })?;
        let opened = s.open_approval(ctx.client_id, command).map_err(|_| {
            invalid("cannot open credential approval: pending limit or randomness failure")
        })?;
        let reply = match s.credentials.insert(
            opened.id.to_string(),
            ctx.client_id,
            provider,
            request,
            opened.ttl,
        ) {
            Ok(reply) => reply,
            Err(message) => {
                s.close_approval(opened.id, ApprovalOutcome::Withdrawn, None);
                return Err(invalid(message));
            }
        };
        Ok((opened, reply))
    })
}

async fn request_values(ctx: &CommandContext, request: CredentialRequest) -> CommandResult {
    let command = Command::Credentials {
        op: CredentialOp::Request(request.clone()),
    };
    let opened = open_request(ctx, request, &command);
    let (opened, mut reply) = match opened {
        Ok(pair) => pair,
        Err(result) => return result,
    };
    let id = opened.id;
    let _cleanup = Cleanup {
        state: ctx.state.clone(),
        id,
    };
    let exchange = async {
        let decision = tokio::select! {
            decision = opened.decision => decision.ok(),
            _ = &mut reply => return denied("1Password provider disconnected"),
        };
        if decision != Some(Decision::Approve) {
            return denied("credential approval denied or withdrawn");
        }
        let ready = ctx.state.with_mut(|s| {
            crate::policy::authorize_command(s, ctx.client_id, &command).is_ok()
                && s.credentials.ready(&id.to_string())
        });
        if !ready {
            return denied("credential requester is no longer authorized");
        }
        match reply.await {
            Ok(Some(values)) => CommandResult::OkWith(CommandValue::CredentialSecrets(values)),
            _ => denied("1Password resolution failed or provider disconnected"),
        }
    };
    let ttl = opened.ttl.min(Duration::from_secs(120));
    tokio::time::timeout(ttl, exchange)
        .await
        .unwrap_or_else(|_| {
            ctx.state
                .with_mut(|s| s.close_approval(id, ApprovalOutcome::Expired, None));
            denied("credential request expired")
        })
}

struct Cleanup {
    state: SharedState,
    id: ApprovalId,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.state.with_mut(|s| {
            s.credentials.remove(&self.id.to_string());
            s.close_approval(self.id, ApprovalOutcome::Withdrawn, None);
        });
    }
}

fn invalid(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::InvalidCommand,
        message: message.to_owned(),
    }
}

fn denied(message: &str) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::PermissionDenied,
        message: message.to_owned(),
    }
}
