//! Pane input credits (ADR-0144): typed input waits for room instead of
//! being dropped.
//!
//! Each Terminal owns a pool of [`INPUT_CREDITS`] permits. A request bound
//! for its encoded-input mailbox holds one from the moment its sender takes
//! it until the PTY writer has written it, so the mailbox (and the writer
//! queue behind it) always has a slot for credited input. A client's read
//! loop takes the credit before handing an event to the input lane and, when
//! the pane is saturated, waits for one: it stops reading that connection,
//! and transport flow control pushes back on the sender. No lock is held
//! while waiting, and the lane itself never waits, so one stalled pane never
//! blocks another pane's input.
//!
//! The wait is bounded by [`INPUT_STALL_LIMIT`]. A pane that drains nothing
//! for that long (its process stopped reading its terminal) refuses the
//! event explicitly, and until it drains again refuses at once, so one wedged
//! pane cannot freeze the rest of the connection.
//!
//! [`INPUT_CREDITS`]: crate::terminal_actor::INPUT_CREDITS
#![allow(
    clippy::redundant_pub_crate,
    reason = "the connection read loop and command handlers use these through the lane's re-export"
)]

use std::collections::HashSet;
use std::time::Duration;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

use crate::state::SharedState;
use crate::terminal_actor::{InputCredit, InputCreditPool, TerminalHandle};

/// How long an event waits for its pane to drain before it is refused.
pub(super) const INPUT_STALL_LIMIT: Duration = Duration::from_secs(5);

/// The pane did not drain within [`INPUT_STALL_LIMIT`]; the event was not
/// delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputStalled;

impl InputStalled {
    /// The wire refusal for `terminal_id`, shared by the uncorrelated
    /// `ERROR` an `INPUT_*` frame gets and a command's `COMMAND_RESULT`.
    pub(crate) fn message(terminal_id: &WireResourceId) -> String {
        format!(
            "input to {terminal_id} refused: the terminal has not drained its input for {}s",
            INPUT_STALL_LIMIT.as_secs()
        )
    }

    pub(crate) fn result(terminal_id: &WireResourceId) -> CommandResult {
        CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: Self::message(terminal_id),
        }
    }
}

/// One connection's credit acquisition, remembering which panes it saw
/// stall so that input to them is refused without another full wait.
#[derive(Debug, Default)]
pub(crate) struct InputCredits {
    stalled: HashSet<WireResourceId>,
}

impl InputCredits {
    /// A credit for `terminal_id`, waiting up to [`INPUT_STALL_LIMIT`] for
    /// one. `Ok(None)` when the id names no local Terminal: the lane then
    /// applies its own not-found and authority handling.
    ///
    /// # Errors
    ///
    /// [`InputStalled`] when the pane did not drain in time, or is still
    /// saturated after an earlier stall on this connection.
    pub(crate) async fn acquire(
        &mut self,
        state: &SharedState,
        terminal_id: &WireResourceId,
    ) -> Result<Option<InputCredit>, InputStalled> {
        let Some(pool) = credit_pool(state, terminal_id) else {
            self.stalled.remove(terminal_id);
            return Ok(None);
        };
        if let Some(credit) = pool.try_take() {
            self.stalled.remove(terminal_id);
            return Ok(Some(credit));
        }
        if self.stalled.contains(terminal_id) {
            return Err(InputStalled);
        }
        crate::perf::INPUT_CREDIT_WAITS.incr();
        let wait_started = std::time::Instant::now();
        let outcome = tokio::time::timeout(INPUT_STALL_LIMIT, pool.take()).await;
        crate::perf::INPUT_CREDIT_WAIT.record_elapsed(wait_started);
        match outcome {
            Ok(credit) => Ok(Some(credit)),
            Err(_elapsed) => {
                crate::perf::INPUT_CREDIT_TIMEOUTS.incr();
                tracing::warn!(
                    ?terminal_id,
                    "pane input stalled; refusing input until it drains"
                );
                self.stalled.insert(terminal_id.clone());
                Err(InputStalled)
            }
        }
    }
}

/// A credit for one event outside a connection's read loop (a held
/// `ROUTE_INPUT`), with the same bounded wait.
///
/// # Errors
///
/// [`InputStalled`] when the pane did not drain in time.
pub(crate) async fn acquire_credit(
    state: &SharedState,
    terminal_id: &WireResourceId,
) -> Result<Option<InputCredit>, InputStalled> {
    InputCredits::default().acquire(state, terminal_id).await
}

/// The credit pool of the local Terminal `terminal_id` names, if any.
fn credit_pool(state: &SharedState, terminal_id: &WireResourceId) -> Option<InputCreditPool> {
    state.with(|s| {
        let core = s.terminal_from_wire(terminal_id)?;
        let terminal = s.resource_handle(core)?.terminal().ok()?;
        Some(terminal.input_credits.clone())
    })
}

/// The credit a handoff to `handle` spends: the one its sender took, when
/// that came from this pane's pool, else one taken now without waiting.
pub(super) fn credit_for(
    handle: &TerminalHandle,
    credit: Option<InputCredit>,
) -> Option<InputCredit> {
    match credit {
        Some(credit) if credit.is_from(&handle.input_credits) => Some(credit),
        _ => handle.input_credits.try_take(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_actor::{INPUT_CREDITS, TerminalActor};

    /// A registered (not running) Terminal and its credit pool.
    fn terminal(state: &SharedState) -> (WireResourceId, InputCreditPool) {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        let pool = bundle
            .handle
            .terminal()
            .expect("facet")
            .input_credits
            .clone();
        let wire = state.with_mut(|s| {
            let pane = s.seed_session("s").2;
            s.register_resource_handle(pane, bundle.handle.clone(), bundle.token.clone())
        });
        (wire, pool)
    }

    /// A pane that drains nothing for the stall limit refuses explicitly,
    /// then refuses at once until a credit frees, then accepts again.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_stalled_pane_refuses_after_the_limit_then_recovers() {
        let state = SharedState::new();
        let (wire, pool) = terminal(&state);
        let mut held: Vec<_> = std::iter::from_fn(|| pool.try_take()).collect();
        assert_eq!(held.len(), INPUT_CREDITS);

        let mut credits = InputCredits::default();
        let started = tokio::time::Instant::now();
        assert_eq!(
            credits.acquire(&state, &wire).await.err(),
            Some(InputStalled)
        );
        assert!(started.elapsed() >= INPUT_STALL_LIMIT);

        let again = tokio::time::Instant::now();
        assert_eq!(
            credits.acquire(&state, &wire).await.err(),
            Some(InputStalled)
        );
        assert_eq!(
            again.elapsed(),
            Duration::ZERO,
            "a known stall refuses at once"
        );

        held.pop();
        let credit = credits.acquire(&state, &wire).await.expect("drained");
        assert!(credit.is_some_and(|credit| credit.is_from(&pool)));
        drop(held);
        assert_eq!(pool.available(), INPUT_CREDITS);
    }

    /// A waiting sender gets the first credit returned, before the limit.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_waiting_sender_takes_the_first_returned_credit() {
        let state = SharedState::new();
        let (wire, pool) = terminal(&state);
        let first = pool.try_take().expect("a free credit");
        let rest: Vec<_> = std::iter::from_fn(|| pool.try_take()).collect();
        assert_eq!(pool.available(), 0);
        let release = async {
            tokio::time::sleep(INPUT_STALL_LIMIT / 2).await;
            drop(first);
        };
        let mut credits = InputCredits::default();
        let (acquired, ()) = tokio::join!(credits.acquire(&state, &wire), release);
        assert!(acquired.expect("no stall").is_some());
        drop(rest);
        assert_eq!(pool.available(), INPUT_CREDITS);
    }

    /// An id naming no local Terminal takes no credit; the lane refuses it.
    #[tokio::test(flavor = "current_thread")]
    async fn an_unknown_terminal_needs_no_credit() {
        let state = SharedState::new();
        let credit = InputCredits::default()
            .acquire(&state, &WireResourceId::local(99))
            .await;
        assert!(matches!(credit, Ok(None)));
    }
}
