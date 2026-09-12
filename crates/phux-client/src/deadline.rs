//! Absolute operation deadlines shared by CLI preparation and client I/O.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// A single time budget, including connection setup, requests, and polling.
#[derive(Clone, Copy, Debug)]
pub struct Deadline {
    /// When the budget started; operation durations are measured from here.
    started: Instant,
    /// When it runs out; `None` is unbounded.
    at: Option<Instant>,
}

impl Deadline {
    /// Start a budget now.
    ///
    /// `None` leaves the operation unbounded, and so does a budget too large
    /// to represent as an instant (`Duration::MAX`, a huge `--timeout`): an
    /// overflow means "never", not a panic.
    #[must_use]
    pub fn new(timeout: Option<Duration>) -> Self {
        let started = Instant::now();
        Self {
            started,
            at: timeout.and_then(|duration| started.checked_add(duration)),
        }
    }

    /// When this budget started.
    #[must_use]
    pub const fn started_at(&self) -> Instant {
        self.started
    }

    /// Whether the budget has already run out.
    #[must_use]
    pub fn expired(&self) -> bool {
        self.at.is_some_and(|at| Instant::now() >= at)
    }

    /// This budget plus `grace`, for work that must not be cut off halfway
    /// once it has begun. An unbounded budget stays unbounded.
    #[must_use]
    pub fn extended(self, grace: Duration) -> Self {
        Self {
            at: self.at.and_then(|at| at.checked_add(grace)),
            ..self
        }
    }

    /// This budget, but never running out sooner than `floor` after it
    /// started. Longer budgets are unchanged.
    #[must_use]
    pub fn floored(self, floor: Duration) -> Self {
        let earliest = self.started.checked_add(floor);
        Self {
            at: self
                .at
                .map(|at| earliest.map_or(at, |earliest| at.max(earliest))),
            ..self
        }
    }

    /// Run a stage within the budget, returning `None` on expiry.
    ///
    /// Expired budgets never poll the stage, so no new input or request is
    /// sent after expiry. Cancellation drops the stage's owned connection.
    pub async fn run<T>(&self, stage: impl Future<Output = T>) -> Option<T> {
        let Some(at) = self.at else {
            return Some(stage.await);
        };
        if Instant::now() >= at {
            return None;
        }
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(at) => None,
            result = stage => Some(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    /// Sets its flag when dropped, so a test can see a stage was cancelled.
    struct DropFlag(Rc<Cell<bool>>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_expired_budget_never_polls_the_stage() {
        let deadline = Deadline::new(Some(Duration::ZERO));
        let polled = Cell::new(false);
        let out = deadline.run(async { polled.set(true) }).await;
        assert!(out.is_none(), "an expired budget reports expiry");
        assert!(!polled.get(), "no request may be sent after expiry");
    }

    #[tokio::test(start_paused = true)]
    async fn no_budget_is_unbounded() {
        let deadline = Deadline::new(None);
        let out = deadline
            .run(async {
                tokio::time::sleep(Duration::from_hours(24)).await;
                7
            })
            .await;
        assert_eq!(out, Some(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_that_finishes_in_time_returns_its_value() {
        let deadline = Deadline::new(Some(Duration::from_secs(5)));
        let out = deadline
            .run(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                "done"
            })
            .await;
        assert_eq!(out, Some("done"));
    }

    #[tokio::test(start_paused = true)]
    async fn an_in_flight_stage_is_cancelled_at_the_deadline() {
        let budget = Duration::from_millis(250);
        let start = Instant::now();
        let deadline = Deadline::new(Some(budget));
        let dropped = Rc::new(Cell::new(false));
        let guard = DropFlag(Rc::clone(&dropped));
        let out = deadline
            .run(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            })
            .await;
        assert!(out.is_none(), "a stage that never answers times out");
        assert!(dropped.get(), "the stage (and its connection) is dropped");
        assert_eq!(
            start.elapsed(),
            budget,
            "expiry lands exactly on the deadline"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_budget_too_large_to_represent_is_unbounded_not_a_panic() {
        // `--timeout 18446744073709551615` and an MCP `timeout_secs` of the
        // same size both land here; `Instant + Duration` used to abort.
        let deadline = Deadline::new(Some(Duration::MAX));
        assert!(!deadline.expired());
        assert_eq!(deadline.run(async { 9 }).await, Some(9));
        assert!(!deadline.extended(Duration::MAX).expired());
        assert!(!deadline.floored(Duration::MAX).expired());
    }

    #[tokio::test(start_paused = true)]
    async fn extended_adds_grace_past_the_deadline() {
        let deadline = Deadline::new(Some(Duration::ZERO));
        assert!(deadline.expired());
        let graced = deadline.extended(Duration::from_secs(2));
        assert!(!graced.expired());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(graced.expired());
        assert!(!Deadline::new(None).extended(Duration::ZERO).expired());
    }

    #[tokio::test(start_paused = true)]
    async fn floored_raises_only_budgets_shorter_than_the_floor() {
        let floor = Duration::from_secs(2);
        let short = Deadline::new(Some(Duration::ZERO)).floored(floor);
        let long = Deadline::new(Some(Duration::from_secs(5))).floored(floor);
        let unbounded = Deadline::new(None).floored(floor);
        assert!(!short.expired(), "a zero budget gets the floor");
        tokio::time::advance(floor).await;
        assert!(short.expired(), "the floor counts from the start");
        assert!(!long.expired(), "a longer budget keeps its own end");
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(long.expired());
        assert!(!unbounded.expired());
    }

    #[tokio::test(start_paused = true)]
    async fn started_at_is_when_the_budget_began() {
        let before = Instant::now();
        let deadline = Deadline::new(Some(Duration::from_secs(1)));
        tokio::time::advance(Duration::from_millis(400)).await;
        assert_eq!(deadline.started_at(), before);
        assert_eq!(deadline.started_at().elapsed(), Duration::from_millis(400));
    }

    #[tokio::test(start_paused = true)]
    async fn stages_share_one_absolute_budget() {
        let deadline = Deadline::new(Some(Duration::from_secs(3)));
        let first = deadline
            .run(tokio::time::sleep(Duration::from_secs(2)))
            .await;
        assert!(first.is_some());
        // Only one second of the original three is left for the next stage.
        let second = deadline
            .run(tokio::time::sleep(Duration::from_secs(2)))
            .await;
        assert!(second.is_none(), "a later stage cannot restart the budget");
    }
}
