//! Ordered, bounded background work for bulk commands. Ordinary resource
//! mutations stay in the connection dispatcher; uploads and transcription can
//! wait on disk/processes without stopping control-frame reception.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const CONNECTION_JOBS: usize = 4;
const CONNECTION_BYTES: usize = 16 * 1024 * 1024;
static GLOBAL_BUDGET: OnceLock<Budget> = OnceLock::new();

#[derive(Clone)]
struct Budget {
    jobs: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Budget {
    fn new(jobs: usize, bytes: usize) -> Self {
        Self {
            jobs: Arc::new(Semaphore::new(jobs)),
            bytes: Arc::new(Semaphore::new(bytes)),
        }
    }

    fn reserve(&self, bytes: u32) -> Result<Reservation, CommandResult> {
        let jobs = self.jobs.clone().try_acquire_owned().map_err(|_| busy())?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| busy())?;
        Ok(Reservation {
            _jobs: jobs,
            _bytes: bytes,
        })
    }
}

struct Reservation {
    _jobs: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

struct Work {
    future: Pin<Box<dyn Future<Output = ()>>>,
    // The future (including its captured payload) drops before admission does.
    _connection: Reservation,
    _global: Reservation,
}

/// One FIFO worker per connection preserves chunk ordering and ensures a
/// `TRANSCRIBE` queued behind final `PUT_FILE` cannot observe an incomplete file.
/// Its `JoinSet` owns cancellation; callers await `shutdown` during teardown.
pub(super) struct CommandTasks {
    tx: mpsc::Sender<Work>,
    worker: JoinSet<()>,
    connection: Budget,
    global: Budget,
}

impl CommandTasks {
    /// A worker for one connection. `cancel` is the connection's token: once
    /// it fires, the job in flight stops at its next await and no queued job
    /// starts, so a revoked connection's queued `PUT_FILE` or `TRANSCRIBE`
    /// never runs (`docs/spec/workload-auth.md` §7 step 2).
    pub(super) fn new(cancel: CancellationToken) -> Self {
        Self::with_budget(
            GLOBAL_BUDGET
                .get_or_init(|| Budget::new(16, 32 * 1024 * 1024))
                .clone(),
            cancel,
        )
    }

    fn with_budget(global: Budget, cancel: CancellationToken) -> Self {
        let (tx, mut rx) = mpsc::channel::<Work>(CONNECTION_JOBS);
        let mut worker = JoinSet::new();
        worker.spawn_local(async move {
            while let Some(mut work) = rx.recv().await {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return,
                    () = work.future.as_mut() => {}
                }
                // Drop completed command captures before accepting another job.
                drop(work);
            }
        });
        Self {
            tx,
            worker,
            connection: Budget::new(CONNECTION_JOBS, CONNECTION_BYTES),
            global,
        }
    }

    /// Returns the retained payload capacity for deliberately selected bulk
    /// commands. `None` means the command must retain ordinary inline ordering.
    pub(super) fn retained_bytes(command: &Command) -> Option<usize> {
        match command {
            Command::PutFile {
                data,
                extension,
                terminal_id,
                ..
            } => Some(
                data.capacity()
                    .saturating_add(extension.capacity())
                    .saturating_add(terminal_id.host().map_or(0, |host| host.as_str().len())),
            ),
            Command::Transcribe { terminal_id, .. } => {
                Some(terminal_id.host().map_or(0, |host| host.as_str().len()))
            }
            _ => None,
        }
    }

    /// `future` must own the command whose capacity was measured by
    /// `retained_bytes`. A refusal drops it without polling or executing it.
    pub(super) fn try_submit(
        &self,
        retained_bytes: usize,
        future: impl Future<Output = ()> + 'static,
    ) -> Result<(), CommandResult> {
        let bytes = u32::try_from(retained_bytes).map_err(|_| busy())?;
        let connection = self.connection.reserve(bytes)?;
        let global = self.global.reserve(bytes)?;
        self.tx
            .try_send(Work {
                future: Box::pin(future),
                _connection: connection,
                _global: global,
            })
            .map_err(|_| busy())
    }

    pub(super) async fn shutdown(&mut self) {
        self.worker.shutdown().await;
    }

    /// Select this alongside connection input. Unexpected worker termination
    /// must end the connection rather than strand already accepted requests.
    pub(super) async fn stopped(&mut self) {
        let _ = self.worker.join_next().await;
    }
}

fn busy() -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::ResourceExhausted,
        message: "bulk command capacity exhausted; wait for pending commands to finish".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[tokio::test]
    async fn queued_commands_keep_order_and_control_can_run() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tasks =
                    CommandTasks::with_budget(Budget::new(4, 1024), CancellationToken::new());
                let (release_tx, release_rx) = tokio::sync::oneshot::channel();
                let first_finished = Rc::new(Cell::new(false));
                let first = first_finished.clone();
                tasks
                    .try_submit(20, async move {
                        release_rx.await.unwrap();
                        first.set(true);
                    })
                    .unwrap();
                let (second_tx, mut second_rx) = tokio::sync::oneshot::channel();
                tasks
                    .try_submit(20, async move {
                        assert!(
                            first_finished.get(),
                            "upload must finish before transcription"
                        );
                        second_tx.send(()).unwrap();
                    })
                    .unwrap();
                tokio::task::yield_now().await;
                assert!(matches!(
                    second_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));
                release_tx.send(()).unwrap();
                second_rx.await.unwrap();
                tasks.shutdown().await;
                drop(tasks);
            })
            .await;
    }

    #[tokio::test]
    async fn admission_counts_active_work_and_shutdown_reclaims_all_captures() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let budget = Budget::new(4, 100);
                let mut tasks = CommandTasks::with_budget(budget.clone(), CancellationToken::new());
                let payload = Rc::new(());
                for _ in 0..4 {
                    let retained = payload.clone();
                    tasks
                        .try_submit(25, async move {
                            std::future::pending::<()>().await;
                            drop(retained);
                        })
                        .unwrap();
                }
                tokio::task::yield_now().await;
                assert_eq!(budget.jobs.available_permits(), 0);
                assert_eq!(budget.bytes.available_permits(), 0);
                assert!(
                    tasks
                        .try_submit(0, async { panic!("refused work ran") })
                        .is_err()
                );
                tasks.shutdown().await;
                drop(tasks);
                assert_eq!(Rc::strong_count(&payload), 1);
                assert_eq!(budget.jobs.available_permits(), 4);
                assert_eq!(budget.bytes.available_permits(), 100);
            })
            .await;
    }

    /// workload-auth §7 step 2: once the connection token fires (live
    /// revocation cancels it), a queued `PUT_FILE` or `TRANSCRIBE` never
    /// starts, even when the job ahead of it would have finished.
    #[tokio::test]
    async fn cancellation_stops_queued_work_before_it_runs() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let cancel = CancellationToken::new();
                let mut tasks = CommandTasks::with_budget(Budget::new(4, 1024), cancel.clone());
                let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
                tasks
                    .try_submit(1, async move {
                        let _ = release_rx.await;
                    })
                    .unwrap();
                let ran = Rc::new(Cell::new(false));
                let queued = ran.clone();
                tasks
                    .try_submit(1, async move {
                        queued.set(true);
                    })
                    .unwrap();
                tokio::task::yield_now().await;
                cancel.cancel();
                drop(release_tx);
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
                assert!(!ran.get(), "a job queued behind cancellation never runs");
                tasks.shutdown().await;
                drop(tasks);
            })
            .await;
    }

    #[tokio::test]
    async fn byte_refusal_rolls_back_job_admission_without_polling() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let budget = Budget::new(4, 100);
                let mut tasks = CommandTasks::with_budget(budget.clone(), CancellationToken::new());
                assert!(
                    tasks
                        .try_submit(101, async { panic!("refused work ran") })
                        .is_err()
                );
                assert_eq!(budget.jobs.available_permits(), 4);
                assert_eq!(tasks.connection.jobs.available_permits(), CONNECTION_JOBS);
                tasks.try_submit(100, async {}).unwrap();
                tasks.shutdown().await;
                drop(tasks);
                assert_eq!(budget.bytes.available_permits(), 100);
            })
            .await;
    }
}
