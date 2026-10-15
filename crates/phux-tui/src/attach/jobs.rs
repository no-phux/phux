//! Background jobs: work the TUI runs off the input loop and reports back
//! as a toast (ADR-0172).
//!
//! An action handler describes the work as a [`Job`] and hands it to the
//! effect pass (`ActionEffects::job`), which calls [`Jobs::start`]: a job of
//! an exclusive [`JobKind`] is refused while another of its kind is in
//! flight, any other is spawned and its starting notice returned. The
//! finished job's [`JobDone`] reaches the driver through [`Jobs::next`], one
//! `select!` arm for every kind of job.
//!
//! The production attach uses one `Jobs` per process ([`Jobs::process`]): a
//! job outlives the attach loop that started it, across session switches and
//! reconnects, so its in-flight guard and its result must too.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tokio::sync::mpsc;

/// What a job is, for its in-flight guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::attach) enum JobKind {
    /// A plugin manifest action (ADR-0017); any number run at once.
    PluginAction,
    /// An organize run or undo (ADR-0170); two at once would race on the
    /// shared undo record, so one at a time.
    Organize,
}

impl JobKind {
    /// The toast refusing a second job of this kind while one runs, or
    /// `None` when the kind is not exclusive.
    fn busy(self) -> Option<Toast> {
        match self {
            Self::PluginAction => None,
            Self::Organize => Some(Toast::new(
                "Organize",
                "An organize run is still in progress; wait for its result.",
            )),
        }
    }
}

/// A toast's title and body lines, built off the input loop (no theme).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct Toast {
    /// The toast title.
    pub(in crate::attach) title: String,
    /// Its body lines.
    pub(in crate::attach) lines: Vec<String>,
}

impl Toast {
    /// A one-line toast.
    pub(in crate::attach) fn new(title: impl Into<String>, line: impl Into<String>) -> Self {
        Self::lines(title, vec![line.into()])
    }

    /// A toast with `lines` as its body.
    pub(in crate::attach) fn lines(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self {
            title: title.into(),
            lines,
        }
    }
}

/// What a finished job asks of the driver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::attach) struct JobDone {
    /// A toast to show, or `None` for a quiet finish.
    pub(in crate::attach) toast: Option<Toast>,
    /// The job changed the session graph: refresh the host inventory and
    /// the chrome that lists sessions.
    pub(in crate::attach) refresh_inventory: bool,
}

impl JobDone {
    /// A finish that shows nothing.
    pub(in crate::attach) fn quiet() -> Self {
        Self::default()
    }

    /// A finish that shows `toast`.
    pub(in crate::attach) const fn toast(toast: Toast) -> Self {
        Self {
            toast: Some(toast),
            refresh_inventory: false,
        }
    }

    /// This finish, also refreshing the host inventory.
    #[must_use]
    pub(in crate::attach) const fn refreshing_inventory(mut self) -> Self {
        self.refresh_inventory = true;
        self
    }
}

type Work = Pin<Box<dyn Future<Output = JobDone> + Send>>;

/// One unit of background work, not yet started.
pub(in crate::attach) struct Job {
    kind: JobKind,
    notice: Option<Toast>,
    work: Work,
}

impl Job {
    /// A `kind` job that runs `work`.
    pub(in crate::attach) fn new(
        kind: JobKind,
        work: impl Future<Output = JobDone> + Send + 'static,
    ) -> Self {
        Self {
            kind,
            notice: None,
            work: Box::pin(work),
        }
    }

    /// This job, announcing itself with `notice` (a passthrough toast) once
    /// it starts.
    #[must_use]
    pub(in crate::attach) fn with_notice(mut self, notice: Toast) -> Self {
        self.notice = Some(notice);
        self
    }

    /// The job's kind.
    #[cfg(test)]
    pub(in crate::attach) const fn kind(&self) -> JobKind {
        self.kind
    }
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("kind", &self.kind)
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

/// What [`Jobs::start`] did with a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) enum Start {
    /// The job runs; show its notice, if it has one.
    Running(Option<Toast>),
    /// Refused: a job of the same exclusive kind is still in flight.
    Busy(Toast),
}

/// The background-job facility: spawns jobs and delivers their results.
/// Cheap to clone; clones share one guard and one result queue.
#[derive(Clone)]
pub(in crate::attach) struct Jobs {
    shared: Arc<Shared>,
}

struct Shared {
    /// Exclusive kinds with a job in flight.
    in_flight: Mutex<HashSet<JobKind>>,
    /// Kept here so the queue never closes while a `Jobs` exists.
    done_tx: mpsc::UnboundedSender<JobDone>,
    done_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<JobDone>>,
}

impl Shared {
    /// Mark `kind` in flight; `false` when it already was.
    fn claim(&self, kind: JobKind) -> bool {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(kind)
    }

    fn release(&self, kind: JobKind) {
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&kind);
    }
}

impl Default for Jobs {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Jobs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jobs").finish_non_exhaustive()
    }
}

impl Jobs {
    /// A facility with nothing in flight.
    pub(in crate::attach) fn new() -> Self {
        let (done_tx, done_rx) = mpsc::unbounded_channel();
        Self {
            shared: Arc::new(Shared {
                in_flight: Mutex::new(HashSet::new()),
                done_tx,
                done_rx: tokio::sync::Mutex::new(done_rx),
            }),
        }
    }

    /// The process's facility, shared by every attach loop it runs.
    pub(in crate::attach) fn process() -> Self {
        static PROCESS: OnceLock<Jobs> = OnceLock::new();
        PROCESS.get_or_init(Self::new).clone()
    }

    /// Spawn `job` on the current runtime, unless its kind is exclusive and
    /// already in flight. Its result arrives through [`Self::next`].
    pub(in crate::attach) fn start(&self, job: Job) -> Start {
        let Job { kind, notice, work } = job;
        if let Some(busy) = kind.busy()
            && !self.shared.claim(kind)
        {
            return Start::Busy(busy);
        }
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let done = work.await;
            shared.release(kind);
            let _ = shared.done_tx.send(done);
        });
        Start::Running(notice)
    }

    /// The next finished job's result; pending while none has finished.
    /// Cancel-safe, so it can sit in a `select!` arm.
    pub(in crate::attach) async fn next(&self) -> JobDone {
        // `shared` holds a sender, so the queue never reports closed.
        match self.shared.done_rx.lock().await.recv().await {
            Some(done) => done,
            None => std::future::pending().await,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// A job that finishes with `done` once `release` fires.
    fn gated(kind: JobKind, done: JobDone) -> (Job, tokio::sync::oneshot::Sender<()>) {
        let (release, gate) = tokio::sync::oneshot::channel::<()>();
        let job = Job::new(kind, async move {
            let _ = gate.await;
            done
        });
        (job, release)
    }

    #[tokio::test]
    async fn an_exclusive_kind_refuses_a_second_job_until_the_first_finishes() {
        let jobs = Jobs::new();
        let notice = Toast::new("Organize", "Organizing…");
        let first_done =
            JobDone::toast(Toast::new("Organized", "1 pane moved")).refreshing_inventory();
        let (first, release) = gated(JobKind::Organize, first_done.clone());
        assert_eq!(
            jobs.start(first.with_notice(notice.clone())),
            Start::Running(Some(notice)),
        );

        let (second, _keep) = gated(JobKind::Organize, JobDone::quiet());
        let Start::Busy(busy) = jobs.start(second) else {
            panic!("a second organize must be refused while the first runs");
        };
        assert_eq!(busy.title, "Organize");
        assert!(busy.lines[0].contains("still in progress"), "{busy:?}");

        release.send(()).expect("first job waiting");
        assert_eq!(
            jobs.next().await,
            first_done,
            "the result routes back whole"
        );

        let (third, release) = gated(JobKind::Organize, JobDone::quiet());
        assert_eq!(
            jobs.start(third),
            Start::Running(None),
            "the guard released"
        );
        release.send(()).expect("third job waiting");
        assert_eq!(jobs.next().await, JobDone::quiet());
    }

    #[tokio::test]
    async fn a_shared_kind_runs_concurrently_and_every_result_arrives() {
        let jobs = Jobs::new();
        let mut gates = Vec::new();
        for n in 0..3 {
            let done = JobDone::toast(Toast::new("plugin", n.to_string()));
            let (job, release) = gated(JobKind::PluginAction, done);
            assert_eq!(jobs.start(job), Start::Running(None));
            gates.push(release);
        }
        for release in gates {
            release.send(()).expect("job waiting");
        }
        let mut lines = Vec::new();
        for _ in 0..3 {
            let done = jobs.next().await;
            lines.extend(done.toast.expect("toast").lines);
        }
        lines.sort();
        assert_eq!(lines, ["0", "1", "2"]);
    }

    #[tokio::test]
    async fn clones_share_the_guard_and_the_queue() {
        let jobs = Jobs::new();
        let other = jobs.clone();
        let (job, release) = gated(JobKind::Organize, JobDone::quiet().refreshing_inventory());
        assert_eq!(jobs.start(job), Start::Running(None));
        let (second, _keep) = gated(JobKind::Organize, JobDone::quiet());
        assert!(matches!(other.start(second), Start::Busy(_)));
        release.send(()).expect("job waiting");
        assert!(other.next().await.refresh_inventory);
    }

    #[test]
    fn the_process_facility_is_one_instance() {
        let a = Jobs::process();
        let b = Jobs::process();
        assert!(Arc::ptr_eq(&a.shared, &b.shared));
    }
}
