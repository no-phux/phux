//! Held commands (ADR-0128, `docs/spec/workload-auth.md` §6.1): the
//! requester's waiter, the decision handler, and withdrawal on disconnect.
//!
//! A held command waits in a task on the requester's own connection, so it
//! runs, when approved, exactly as the requester's dispatch would have run
//! it: the requester's client id, `request_id`, mailbox, and tokens, and a
//! fresh check against the requester's grant under the then-current
//! topology. The approver only ends the approval; its grant is never used to
//! run anything.

use std::time::Duration;

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities};
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{
    APPROVAL_APPROVE, APPROVAL_DECIDE_KEY_PREFIX, APPROVAL_DENY, ApprovalOutcome, Command,
    CommandResult, ErrorCode, FrameKind, Scope,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::input_lane::InputLaneHandle;
use crate::state::{ClientId, Decision, HoldRefusal, OpenedApproval, Outbound, SharedState};

/// Everything a held command needs to run later as the requester's own
/// dispatch would have run it.
pub(super) struct HeldContext {
    pub(super) state: SharedState,
    pub(super) client_id: ClientId,
    pub(super) out_tx: mpsc::Sender<Outbound>,
    pub(super) client_caps: ClientCapabilities,
    pub(super) profile: BootstrapProfile,
    pub(super) limits: BootstrapLimits,
    pub(super) input_lane: Option<InputLaneHandle>,
    pub(super) token: CancellationToken,
    pub(super) root_token: CancellationToken,
    pub(super) defer_subscription: bool,
}

/// How a held command's wait ended.
enum Ending {
    Decided(Decision),
    Expired,
    /// The connection closed or the approval was withdrawn: nobody is owed
    /// a result.
    Gone,
}

/// Hold `command`: open its approval and spawn the waiter that ends it. The
/// `COMMAND_RESULT` is deferred; a refusal to hold answers at once.
pub(super) async fn hold_command(
    ctx: HeldContext,
    waiters: &mut JoinSet<()>,
    request_id: u32,
    command: Command,
) {
    // A keyed retry is never held twice: a key that already has an answer
    // replays it, and a repeat of a pending hold joins it.
    if let Some(result) = super::keyed_ops::prior_answer(&ctx.state, &command).await {
        let _ = ctx
            .out_tx
            .send(Outbound::Frame(FrameKind::CommandResult {
                request_id,
                result,
            }))
            .await;
        return;
    }
    match ctx
        .state
        .with_mut(|s| s.join_approval(ctx.client_id, &command))
    {
        Ok(Some(decision)) => {
            waiters.spawn_local(wait_joined(ctx, request_id, command, decision));
            return;
        }
        Err(refusal) => {
            let (code, message) = refusal_reply(refusal);
            reply(&ctx.out_tx, request_id, code, message).await;
            return;
        }
        Ok(None) => {}
    }
    let opened = ctx
        .state
        .with_mut(|s| s.open_approval(ctx.client_id, &command));
    match opened {
        Ok(opened) => {
            waiters.spawn_local(wait_for_decision(ctx, request_id, command, opened));
        }
        Err(refusal) => {
            let (code, message) = refusal_reply(refusal);
            reply(&ctx.out_tx, request_id, code, message).await;
        }
    }
}

const fn refusal_reply(refusal: HoldRefusal) -> (ErrorCode, &'static str) {
    match refusal {
        HoldRefusal::TooManyPending => (
            ErrorCode::ResourceExhausted,
            "too many actions awaiting approval",
        ),
        HoldRefusal::ServerFull => (
            ErrorCode::ResourceExhausted,
            "too many actions awaiting approval on this server",
        ),
        HoldRefusal::NoRandomness => (ErrorCode::InternalError, "could not mint an approval id"),
    }
}

async fn wait_for_decision(
    ctx: HeldContext,
    request_id: u32,
    command: Command,
    opened: OpenedApproval,
) {
    let OpenedApproval {
        id,
        mut decision,
        ttl,
    } = opened;
    // However the wait ends (the connection closing aborts this task), a
    // still-pending approval is withdrawn. After any other ending the
    // approval is already closed and this does nothing.
    let _withdraw = WithdrawOnDrop {
        state: ctx.state.clone(),
        id,
    };
    let ending = await_ending(&ctx, id, ttl, &mut decision).await;
    finish(&ctx, request_id, command, ending).await;
}

/// A request that joined a pending hold: it waits for the same decision,
/// and the hold's own waiter owns the expiry and the withdrawal.
async fn wait_joined(
    ctx: HeldContext,
    request_id: u32,
    command: Command,
    mut decision: oneshot::Receiver<Decision>,
) {
    // However the waiter ends (the connection closing aborts this task), its
    // count against the per-connection bound is released.
    let _release = ReleaseJoinOnDrop {
        state: ctx.state.clone(),
        requester: ctx.client_id,
    };
    let ending = tokio::select! {
        biased;
        decided = &mut decision => decided.map_or(Ending::Gone, Ending::Decided),
        () = ctx.token.cancelled() => Ending::Gone,
    };
    finish(&ctx, request_id, command, ending).await;
}

/// Answer the requester as the ending says: run the approved command, or
/// refuse it with the reason. A keyed command that two waiters approved
/// runs once; L20's dedupe answers the other.
async fn finish(ctx: &HeldContext, request_id: u32, command: Command, ending: Ending) {
    let refusal = match ending {
        Ending::Decided(Decision::Approve) => return run_approved(ctx, request_id, command).await,
        Ending::Gone => return,
        Ending::Decided(Decision::Deny) => "approval denied",
        Ending::Decided(Decision::TerminalGone) => "terminal gone",
        Ending::Expired | Ending::Decided(Decision::Expired) => "approval expired",
    };
    reply(
        &ctx.out_tx,
        request_id,
        ErrorCode::PermissionDenied,
        refusal,
    )
    .await;
}

async fn await_ending(
    ctx: &HeldContext,
    id: ApprovalId,
    ttl: Duration,
    decision: &mut oneshot::Receiver<Decision>,
) -> Ending {
    // A decision already delivered wins over a cancellation observed at the
    // same poll: the approval was journaled, so the command runs once (its
    // re-authorization still refuses a revoked requester).
    tokio::select! {
        biased;
        decided = &mut *decision => decided.map_or(Ending::Gone, Ending::Decided),
        () = ctx.token.cancelled() => Ending::Gone,
        () = tokio::time::sleep(ttl) => expire_or_take(&ctx.state, id, decision).await,
    }
}

/// The TTL elapsed: expire the approval, unless a decision took it first,
/// in which case that decision is already on its way.
async fn expire_or_take(
    state: &SharedState,
    id: ApprovalId,
    decision: &mut oneshot::Receiver<Decision>,
) -> Ending {
    let expired = state.with_mut(|s| s.close_approval(id, ApprovalOutcome::Expired, None));
    if let Some(pending) = expired {
        // Joined waiters hear the expiry too.
        pending.deliver(Decision::Expired);
        return Ending::Expired;
    }
    decision.await.map_or(Ending::Gone, Ending::Decided)
}

/// Run an approved command once, as the requester, after classifying it
/// again under the requester's current grant and topology. A Terminal that
/// moved out of the grant since, or a grant revoked since, is refused.
async fn run_approved(ctx: &HeldContext, request_id: u32, command: Command) {
    let admitted = ctx
        .state
        .with(|s| crate::policy::authorize_command(s, ctx.client_id, &command))
        .is_ok();
    if !admitted {
        reply(
            &ctx.out_tx,
            request_id,
            ErrorCode::PermissionDenied,
            "permission denied",
        )
        .await;
        return;
    }
    // Held commands are SIGNAL-class, which never route to the input lane
    // or the bulk worker: the shared route says the handler, as it would
    // for the same command unheld.
    debug_assert_eq!(
        super::client::route(&command, ctx.input_lane.is_some()),
        super::client::Route::Handler
    );
    super::commands::handle_command(
        &ctx.state,
        ctx.client_id,
        request_id,
        command,
        &ctx.out_tx,
        ctx.client_caps,
        ctx.profile,
        ctx.limits,
        ctx.input_lane.as_ref(),
        &ctx.token,
        &ctx.root_token,
        ctx.defer_subscription,
    )
    .await;
}

/// The deferred result waits for mailbox capacity rather than being
/// dropped; a closed connection is owed nothing.
async fn reply(out_tx: &mpsc::Sender<Outbound>, request_id: u32, code: ErrorCode, message: &str) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code,
                message: message.to_owned(),
            },
        }))
        .await;
}

struct ReleaseJoinOnDrop {
    state: SharedState,
    requester: ClientId,
}

impl Drop for ReleaseJoinOnDrop {
    fn drop(&mut self) {
        self.state.with_mut(|s| s.release_joined(self.requester));
    }
}

struct WithdrawOnDrop {
    state: SharedState,
    id: ApprovalId,
}

impl Drop for WithdrawOnDrop {
    fn drop(&mut self) {
        let _ = self
            .state
            .with_mut(|s| s.close_approval(self.id, ApprovalOutcome::Withdrawn, None));
    }
}

/// Whether a `SET_METADATA` is a decision the server intercepts.
pub(super) fn is_decision(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Global) && key.starts_with(APPROVAL_DECIDE_KEY_PREFIX)
}

/// `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }`: end one held
/// action. The dispatch guard already judged the decider; this ends the
/// approval once, journaled with the decider as actor, and hands the
/// decision to the requester's waiter. Nothing is stored under the key.
pub(super) async fn decide(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    key: &str,
    value: &[u8],
    out_tx: &mpsc::Sender<Outbound>,
) {
    let Some((id, decision)) = parse_decision(key, value) else {
        refuse(
            out_tx,
            request_id,
            ErrorCode::InvalidCommand,
            "malformed approval decision",
        )
        .await;
        return;
    };
    let pending = state.with_mut(|s| s.close_approval(id, decision.outcome(), Some(client_id)));
    match pending {
        Some(pending) => pending.deliver(decision),
        None => {
            refuse(
                out_tx,
                request_id,
                ErrorCode::PermissionDenied,
                "no pending approval",
            )
            .await;
        }
    }
}

fn parse_decision(key: &str, value: &[u8]) -> Option<(ApprovalId, Decision)> {
    let id = ApprovalId::from_decide_key(key)?;
    let decision = match value {
        APPROVAL_APPROVE => Decision::Approve,
        APPROVAL_DENY => Decision::Deny,
        _ => return None,
    };
    Some((id, decision))
}

/// A refused decision's correlated `ERROR`.
async fn refuse(out_tx: &mpsc::Sender<Outbound>, request_id: u32, code: ErrorCode, message: &str) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::Error {
            request_id: Some(request_id),
            code,
            message: message.to_owned(),
        }))
        .await;
}

/// Withdraw every action `client` holds; its connection is closing.
pub(super) fn withdraw(state: &SharedState, client: ClientId) {
    let _ = state.with_mut(|s| s.withdraw_approvals(client));
}
