//! Shared per-generation state for the pane output pumps.
//!
//! Two pumps forward one pane's broadcast output to one consumer: the ATTACH
//! pump in [`crate::runtime::attach`] and the `ATTACH_RESOURCE` pump in
//! [`crate::runtime::commands`]. They differ in how they publish a bootstrap
//! and in what they do when they fail, but the rules for *what may go on the
//! wire right now* are identical — and when they were written twice, only one
//! copy got the gap fence (phux-l96p.10), so the same client-killing sequence
//! gap stayed live on the `rec` / `play` / headless / FFI path after it was
//! fixed for interactive attach.
//!
//! [`PumpGeneration`] is that shared rule set, and it keeps `generation_active`
//! and `gap_pending` **private**: a pump cannot put a live delta on the wire
//! without asking [`PumpGeneration::forwards`], so a third pump cannot
//! reintroduce the bug by forgetting a flag it never sees.

use std::time::Duration;

use phux_protocol::ids::BootstrapId;

use crate::terminal_actor::{PaneOutput, ResyncAudience, ResyncTarget};

/// Spawn an owned output task from any subscription path. Completion guards
/// are captured before spawning, so even abort-before-first-poll resolves the
/// detach fence. A `JoinSet` owner additionally retains whole-session teardown.
pub(super) fn spawn_tracked(
    state: &crate::state::SharedState,
    client: crate::state::ClientId,
    terminal: phux_core::ResourceId,
    tasks: Option<&mut tokio::task::JoinSet<()>>,
    future: impl std::future::Future<Output = ()> + 'static,
) {
    let done = tokio_util::sync::CancellationToken::new();
    let guard = done.clone().drop_guard();
    let task = async move {
        let _guard = guard;
        future.await;
    };
    let abort = match tasks {
        Some(tasks) => {
            // Completed SPAWN pumps must not accumulate in the connection's
            // JoinSet during create/detach churn.
            while tasks.try_join_next().is_some() {}
            tasks.spawn_local(task)
        }
        None => tokio::task::spawn_local(task).abort_handle(),
    };
    state.with_mut(|s| s.track_terminal_output_pump(client, terminal, abort, done));
}

/// Stop all output tasks for a subscription, including tasks blocked inside
/// bootstrap publication or a send to a full consumer mailbox.
pub(super) async fn stop_output(
    state: &crate::state::SharedState,
    client: crate::state::ClientId,
    terminal: phux_core::ResourceId,
) {
    let completions = state.with_mut(|s| s.stop_terminal_output_pumps(client, terminal));
    for done in completions {
        done.cancelled().await;
    }
}

/// How long a fenced pump waits for the replacement generation before asking
/// for it again, the first time.
///
/// An order of magnitude above the actor's `RESIZE_RESYNC_DEBOUNCE`, so a
/// resync that is merely coalescing is never mistaken for one that was lost.
const GAP_RESYNC_RETRY: Duration = Duration::from_millis(500);

/// Ceiling on the doubling backoff between retries.
///
/// The backoff exists because the actor coalesces gap resyncs behind one
/// debounce: N fenced pumps on one pane all retrying on the same fixed period
/// arrive at a mean interval of `period / N`, which at ten consumers is faster
/// than the debounce can fire. Doubling pulls the fleet apart instead of
/// hammering in lockstep.
const GAP_RESYNC_MAX_BACKOFF: Duration = Duration::from_secs(4);

/// How many resync requests one gap gets before it is declared unrecoverable.
///
/// A fenced pump forwards nothing, so an actor that accepts the request and
/// never answers it would otherwise hold the consumer on a frozen screen
/// forever while logging a warning twice a second. The budget turns that into
/// a bounded wait — ~7.5s across the doubling steps below — ending in a
/// terminal `ERROR` the consumer can reconnect from. Generous enough that a
/// merely busy actor is never mistaken for a dead one.
const GAP_RESYNC_MAX_ATTEMPTS: u32 = 5;

/// How long after the pane read a chunk an interactive pump may still forward
/// it.
///
/// The broadcast holds 256 chunks, so its own `Lagged` signal fires only once
/// a consumer is hundreds of kilobytes behind — on a remote link slower than
/// the pane's output, that is ten seconds of screen in front of every
/// keystroke echo. On a local socket a chunk reaches the pump within
/// milliseconds; one that is older than this is a consumer draining slower
/// than the pane talks, and it gets one fresh screen instead of the backlog.
///
/// The clock starts at the later of the PTY read and the current
/// generation's publication ([`PumpGeneration::chunk_age`]). A pump is
/// blocked while its bootstrap drains, so without that anchor the first live
/// chunk after a slow-but-healthy republish would already look stale and
/// start another resync — a consumer that only ever sees checkpoints.
pub(super) const STALE_OUTPUT_BUDGET: Duration = Duration::from_millis(250);

/// Has a chunk read `age` ago fallen past [`STALE_OUTPUT_BUDGET`]?
pub(super) fn is_stale(age: Duration) -> bool {
    age > STALE_OUTPUT_BUDGET
}

/// Where one pump has got to inside the generation it is publishing, and
/// whether that generation may still carry live output.
#[derive(Debug)]
pub(super) struct PumpGeneration {
    /// Highest raw sequence already covered by the published bootstrap.
    published_cut: u64,
    /// Highest raw sequence actually forwarded to the consumer.
    last_forwarded_seq: u64,
    /// Generation every frame this pump emits is labelled with.
    bootstrap_id: BootstrapId,
    /// Cleared by a tombstone and set again once a replacement bootstrap is
    /// published; nothing may be forwarded in between.
    generation_active: bool,
    /// Set the moment the broadcast drops a window under this pump, cleared
    /// when the replacement generation is published.
    ///
    /// While it is set the pump forwards nothing. Two things depend on that.
    /// First, the consumer's mirror is exactly sequenced: a `RESOURCE_OUTPUT`
    /// whose `seq` skips the dropped window is a `SequenceGap`, which the
    /// client kernel treats as a protocol error and detaches on — so
    /// forwarding "the rest" after a gap does not degrade the session, it ends
    /// it. Second, a pump that keeps awaiting mailbox capacity for frames the
    /// consumer cannot use drains the broadcast at the *consumer's* speed, and
    /// the in-band resync it just asked for is delivered on that same
    /// broadcast: at PTY speed the resync is overwritten before the pump
    /// reaches it, and the next lag re-arms the same trap. Dropping instead of
    /// queueing lets the pump drain at memory speed, so the resync always
    /// arrives. This is tmux's rule — a consumer far enough behind gets one
    /// fresh screen, not a replay of everything it missed.
    gap_pending: bool,
    /// Resync requests already spent on the current gap; reset when a
    /// replacement generation lands. Bounded by [`GAP_RESYNC_MAX_ATTEMPTS`].
    gap_attempts: u32,
    /// When the current generation was published (opened or republished):
    /// the earliest instant [`Self::chunk_age`] measures from.
    published_at: std::time::Instant,
}

impl PumpGeneration {
    /// Start at the cut the publication gate handed over.
    pub(super) fn opened_at(published_cut: u64, bootstrap_id: BootstrapId) -> Self {
        Self {
            published_cut,
            last_forwarded_seq: published_cut,
            bootstrap_id,
            generation_active: true,
            gap_pending: false,
            gap_attempts: 0,
            published_at: std::time::Instant::now(),
        }
    }

    /// How stale a live chunk read at `read_at` is for this consumer: the time
    /// since the later of that read and this generation's publication.
    ///
    /// A chunk read before the generation was published waited behind the
    /// bootstrap, not behind a slow consumer; counting that wait would resync
    /// a healthy consumer again the moment its republish finished draining.
    pub(super) fn chunk_age(&self, read_at: std::time::Instant) -> Duration {
        std::time::Instant::now().saturating_duration_since(read_at.max(self.published_at))
    }

    /// The generation label every frame this pump emits carries.
    pub(super) const fn bootstrap_id(&self) -> BootstrapId {
        self.bootstrap_id
    }

    /// Adopt the next generation label ahead of republishing.
    pub(super) const fn set_bootstrap_id(&mut self, bootstrap_id: BootstrapId) {
        self.bootstrap_id = bootstrap_id;
    }

    /// Highest raw sequence actually forwarded, for a tombstone's
    /// `last_valid_seq`.
    pub(super) const fn last_forwarded_seq(&self) -> u64 {
        self.last_forwarded_seq
    }

    /// Is a generation currently published and unretired?
    pub(super) const fn is_active(&self) -> bool {
        self.generation_active
    }

    /// Is this pump waiting on a replacement generation after a gap?
    pub(super) const fn is_fenced(&self) -> bool {
        self.gap_pending
    }

    /// May this live delta go on the wire?
    ///
    /// The single gate every pump's live-forward path must pass through. A
    /// retired generation and a fenced one both answer `false`, as does a
    /// sequence the published bootstrap already covers.
    pub(super) const fn forwards(&self, seq: u64) -> bool {
        self.generation_active && !self.gap_pending && seq > self.published_cut
    }

    /// Record a delta that reached the consumer.
    pub(super) const fn note_forwarded(&mut self, seq: u64) {
        self.last_forwarded_seq = seq;
    }

    /// A tombstone retired the published generation; nothing may be forwarded
    /// until a replacement is published.
    pub(super) const fn retire(&mut self) {
        self.generation_active = false;
    }

    /// The broadcast dropped a window under this pump: fence the generation
    /// before asking for a resync.
    ///
    /// Returns whether a resync was *already* in flight, which distinguishes a
    /// fresh gap (worth a `WARN`) from a repeat while fenced (a `DEBUG`, so a
    /// pane that keeps lagging cannot flood the log).
    pub(super) const fn fence_for_gap(&mut self) -> bool {
        let already_pending = self.gap_pending;
        self.gap_pending = true;
        already_pending
    }

    /// Record that a resync request went out for the current gap.
    pub(super) const fn note_resync_requested(&mut self) {
        self.gap_attempts = self.gap_attempts.saturating_add(1);
    }

    /// How long to wait for the replacement generation before asking again,
    /// or `None` once this gap has spent its budget.
    ///
    /// Doubles from [`GAP_RESYNC_RETRY`] to [`GAP_RESYNC_MAX_BACKOFF`].
    fn gap_retry_delay(&self) -> Option<Duration> {
        if self.gap_attempts >= GAP_RESYNC_MAX_ATTEMPTS {
            return None;
        }
        let step = self.gap_attempts.saturating_sub(1).min(u32::BITS - 1);
        Some(
            GAP_RESYNC_RETRY
                .saturating_mul(1_u32 << step)
                .min(GAP_RESYNC_MAX_BACKOFF),
        )
    }

    /// How many resync requests this gap has already cost, for the log line
    /// that gives up on it.
    pub(super) const fn gap_attempts(&self) -> u32 {
        self.gap_attempts
    }

    /// Does a [`PaneOutput::Resync`] addressed to `audience` replace this
    /// pump's generation?
    ///
    /// The single gate both pumps' resync arms pass through, for the same
    /// reason [`Self::forwards`] is the single live gate. A resync addressed
    /// to everyone (a reflow) always does. One addressed to named pumps is a
    /// gap resync some pump asked for: it replaces this generation only if
    /// this pump is named *and still fenced*. Every other pump on the pane —
    /// the local TUI beside a slow remote attach, a recorder, a cockpit —
    /// keeps its generation and pays no tombstone, no bootstrap, and no
    /// native checkpoint capture (phux-auqy). A named pump that is no longer
    /// fenced already took an everyone-resync that healed its gap, so a second
    /// republish would be exactly that churn again.
    ///
    /// Taking an addressed resync only while fenced loses nothing: a pump
    /// asks for one only after fencing itself, and only a republish unfences.
    ///
    /// A *retired* generation — a forwarded `BootstrapTombstone` voided it,
    /// with no gap fence set — takes any resync, addressed to it or not. It
    /// forwards nothing until a replacement lands and never asks for one of
    /// its own (the stale and lag paths both go through [`Self::forwards`],
    /// which a retired generation fails), so another pump's resync is its
    /// only way back. Taking it is always safe: the resync is an ordered cut
    /// on the same broadcast, covering every sequence before it. Before
    /// resyncs were addressed, any everyone-resync revived such a pump; this
    /// keeps that rescue.
    pub(super) fn takes_resync(&self, audience: &ResyncAudience, pump: ResyncTarget) -> bool {
        match audience {
            ResyncAudience::Everyone => true,
            ResyncAudience::Only(_) if !self.generation_active => true,
            ResyncAudience::Only(_) => self.gap_pending && audience.includes(pump),
        }
    }

    /// A replacement generation is published at `base_seq`: unfence, reactivate
    /// and re-anchor the sequence expectation.
    pub(super) fn republished_at(&mut self, base_seq: u64) {
        self.published_cut = base_seq;
        self.last_forwarded_seq = base_seq;
        self.generation_active = true;
        self.gap_pending = false;
        self.gap_attempts = 0;
        self.published_at = std::time::Instant::now();
    }
}

/// What one turn of waiting on the pane's broadcast produced.
pub(super) enum PumpWait {
    /// Dispatch this broadcast result.
    Event(Result<PaneOutput, tokio::sync::broadcast::error::RecvError>),
    /// A fenced pump's backoff elapsed with no replacement generation: ask
    /// again.
    RetryResync,
    /// The gap spent its whole request budget without an answer. The pump
    /// must tell the consumer and stop, rather than hold it on a screen that
    /// can never change.
    GapUnrecoverable,
}

/// The next broadcast event for a pump.
///
/// A pump that is not fenced simply awaits the broadcast. A fenced one bounds
/// that wait: it is forwarding nothing until the replacement generation lands,
/// so without a bound a resync that never arrived would be indistinguishable
/// from a pane with nothing to say — and a bound with no budget behind it is
/// just an infinite retry loop, which is what this used to be.
///
/// # Cancel safety
///
/// `broadcast::Receiver::recv` is cancel-safe, so the bounded wait cannot drop
/// a message it had already taken.
pub(super) async fn next_event(
    generation: &PumpGeneration,
    output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
) -> PumpWait {
    if !generation.is_fenced() {
        return PumpWait::Event(output_rx.recv().await);
    }
    let Some(delay) = generation.gap_retry_delay() else {
        return PumpWait::GapUnrecoverable;
    };
    let Ok(received) = tokio::time::timeout(delay, output_rx.recv()).await else {
        return PumpWait::RetryResync;
    };
    PumpWait::Event(received)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        GAP_RESYNC_MAX_ATTEMPTS, GAP_RESYNC_RETRY, PumpGeneration, PumpWait, STALE_OUTPUT_BUDGET,
        is_stale, next_event,
    };

    use crate::terminal_actor::{PaneOutput, ResyncAudience, ResyncTarget};

    fn pump_on(owner: u64, stream: u64) -> ResyncTarget {
        ResyncTarget {
            owner,
            stream_id: phux_protocol::ids::StreamId::new(stream).expect("non-zero stream id"),
        }
    }

    /// phux-auqy: an addressed gap resync replaces only the fenced pump it
    /// names. A fresh pump beside it — or the same client on another stream —
    /// keeps its generation; a reflow still replaces everyone's.
    #[test]
    fn an_addressed_resync_is_taken_only_by_the_fenced_pump_it_names() {
        let stale = pump_on(1, 1);
        let addressed = ResyncAudience::Only(vec![stale].into());

        let mut fenced = opened();
        fenced.fence_for_gap();
        assert!(fenced.takes_resync(&addressed, stale));
        assert!(
            !fenced.takes_resync(&addressed, pump_on(1, 2)),
            "another stream of the same client is a different pump",
        );

        let fresh = opened();
        assert!(
            !fresh.takes_resync(&addressed, pump_on(2, 1)),
            "a fresh pump must not be re-bootstrapped for someone else's gap",
        );
        assert!(
            !fresh.takes_resync(&addressed, stale),
            "a named pump whose gap an earlier resync already healed skips it",
        );

        assert!(fresh.takes_resync(&ResyncAudience::Everyone, pump_on(2, 1)));
        assert!(fenced.takes_resync(&ResyncAudience::Everyone, stale));
    }

    /// A retired generation takes any resync, addressed or not.
    ///
    /// A tombstone can retire a pump without fencing it, and such a pump asks
    /// for no resync of its own; before resyncs were addressed, whichever
    /// everyone-resync came next revived it. Another pump's addressed resync
    /// must still do so, or the retired pump never shows output again.
    #[test]
    fn a_retired_generation_takes_any_resync_even_one_addressed_elsewhere() {
        let addressed_elsewhere = ResyncAudience::Only(vec![pump_on(1, 1)].into());
        let mut retired = opened();
        retired.retire();
        assert!(!retired.is_fenced(), "retirement alone sets no gap fence");
        assert!(
            retired.takes_resync(&addressed_elsewhere, pump_on(2, 1)),
            "the stale neighbour's resync is the retired pump's only way back",
        );
        assert!(retired.takes_resync(&ResyncAudience::Everyone, pump_on(2, 1)));

        retired.republished_at(100);
        assert!(
            !retired.takes_resync(&addressed_elsewhere, pump_on(2, 1)),
            "once republished it is an ordinary fresh pump again",
        );
    }

    /// A chunk that waited behind a republish is measured from the republish.
    ///
    /// Read two seconds ago but dequeued straight after its generation was
    /// published, it is fresh: the wait was the bootstrap draining, not the
    /// consumer falling behind. Without the anchor a slow-but-healthy link
    /// would resync on every republish and never show live output.
    #[test]
    fn chunk_age_starts_no_earlier_than_the_generation_publication() {
        let read_long_ago = std::time::Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("monotonic clock has run for two seconds");
        let mut generation = opened();
        generation.republished_at(100);
        assert!(!is_stale(generation.chunk_age(read_long_ago)));
        assert!(
            generation.chunk_age(std::time::Instant::now()) < STALE_OUTPUT_BUDGET,
            "a chunk read after publication is aged from its own read",
        );
    }

    #[test]
    fn output_is_stale_only_past_the_budget() {
        assert!(!is_stale(Duration::ZERO));
        assert!(!is_stale(STALE_OUTPUT_BUDGET));
        assert!(is_stale(STALE_OUTPUT_BUDGET + Duration::from_millis(1)));
    }

    #[test]
    fn tracked_pump_abort_fences_blocked_send_and_reclaims_churn() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let state = crate::state::SharedState::new();
            let client = crate::state::ClientId(1);
            let terminal = phux_core::ResourceId::default();
            let mut tasks = tokio::task::JoinSet::new();
            let (mailbox, mut receive) = tokio::sync::mpsc::channel(1);
            let (output, _) = tokio::sync::broadcast::channel::<()>(1);
            mailbox.send(0).await.unwrap();
            for round in 1..=40 {
                let subscription = output.subscribe();
                let sender = mailbox.clone();
                let (entered, entry) = tokio::sync::oneshot::channel();
                super::spawn_tracked(&state, client, terminal, Some(&mut tasks), async move {
                    let _subscription = subscription;
                    entered.send(()).unwrap();
                    sender.send(round).await.unwrap();
                });
                entry.await.unwrap();
                assert_eq!(output.receiver_count(), 1);
                tokio::time::timeout(
                    Duration::from_secs(1),
                    super::stop_output(&state, client, terminal),
                )
                .await
                .unwrap();
                assert_eq!(
                    output.receiver_count(),
                    0,
                    "detach dropped the live receiver"
                );
                assert!(tasks.len() <= 1, "completed JoinSet tasks accumulated");
                assert!(
                    state
                        .with_mut(|s| s.stop_terminal_output_pumps(client, terminal))
                        .is_empty()
                );
            }
            assert_eq!(receive.recv().await, Some(0));
            assert!(
                receive.try_recv().is_err(),
                "aborted send escaped the detach fence"
            );
            while tasks.join_next().await.is_some() {}
            assert!(tasks.is_empty());
        });
    }

    #[test]
    fn tracked_pump_can_be_detached_before_its_first_poll() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let state = crate::state::SharedState::new();
            let client = crate::state::ClientId(1);
            let terminal = phux_core::ResourceId::default();
            let (output, _) = tokio::sync::broadcast::channel::<()>(1);
            let receiver = output.subscribe();
            super::spawn_tracked(&state, client, terminal, None, async move {
                let _receiver = receiver;
                panic!("aborted task must never run");
            });
            tokio::time::timeout(
                Duration::from_secs(1),
                super::stop_output(&state, client, terminal),
            )
            .await
            .unwrap();
            assert_eq!(output.receiver_count(), 0);
        });
    }

    fn bootstrap(raw: u64) -> phux_protocol::ids::BootstrapId {
        phux_protocol::ids::BootstrapId::new(raw).expect("non-zero bootstrap id")
    }

    fn opened() -> PumpGeneration {
        PumpGeneration::opened_at(41, bootstrap(7))
    }

    #[test]
    fn a_fresh_generation_forwards_only_past_its_cut() {
        let generation = opened();
        assert!(
            !generation.forwards(41),
            "the cut itself is already covered"
        );
        assert!(generation.forwards(42));
        assert!(!generation.is_fenced());
    }

    #[test]
    fn a_gap_fences_every_live_delta_until_the_replacement_lands() {
        let mut generation = opened();
        assert!(!generation.fence_for_gap(), "first gap is not a repeat");
        assert!(generation.is_fenced());
        // This is the frame that used to detach the client.
        assert!(!generation.forwards(20_533));
        assert!(generation.fence_for_gap(), "second gap is a repeat");

        generation.republished_at(9_000);
        assert!(!generation.is_fenced());
        assert!(!generation.forwards(9_000));
        assert!(generation.forwards(9_001));
    }

    #[test]
    fn a_retired_generation_forwards_nothing_even_unfenced() {
        let mut generation = opened();
        generation.retire();
        assert!(!generation.is_active());
        assert!(!generation.forwards(42));
        generation.republished_at(100);
        assert!(generation.is_active());
        assert!(generation.forwards(101));
    }

    #[test]
    fn the_retry_window_sits_well_above_the_actor_resync_debounce() {
        assert!(
            GAP_RESYNC_RETRY >= crate::terminal_actor::RESIZE_RESYNC_DEBOUNCE * 4,
            "a resync that is merely coalescing must not look like one that was lost",
        );
    }

    /// The fence is a bounded wait, not an infinite retry loop.
    ///
    /// Every retry doubles, so N pumps on one pane pull apart instead of
    /// hammering the actor's coalescing debounce in lockstep, and the budget
    /// runs out — an actor that accepts a resync and never broadcasts one must
    /// end in a terminal error the consumer can reconnect from, not a frozen
    /// screen and two warnings a second forever.
    #[test]
    fn a_gap_retries_with_backoff_and_then_gives_up() {
        let mut generation = opened();
        generation.fence_for_gap();

        let mut delays = Vec::new();
        loop {
            generation.note_resync_requested();
            match generation.gap_retry_delay() {
                Some(delay) => delays.push(delay),
                None => break,
            }
            assert!(
                delays.len() < 32,
                "the fence budget must be finite; got {delays:?}",
            );
        }

        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
            "doubling backoff, capped at GAP_RESYNC_MAX_BACKOFF",
        );
        assert_eq!(generation.gap_attempts(), GAP_RESYNC_MAX_ATTEMPTS);
        let total: Duration = delays.iter().sum();
        assert!(
            total >= Duration::from_secs(5) && total <= Duration::from_secs(30),
            "the whole fence must be bounded and humane, got {total:?}",
        );
    }

    /// The whole point of item 2: a publication replay may carry entries the
    /// checkpoint it accompanies already covers, and re-sending one under the
    /// new `bootstrap_id` is a `DuplicateSequence` to the client kernel, which
    /// detaches on it. Both native replay loops filter on exactly this.
    #[test]
    fn a_replay_entry_at_or_behind_the_cut_is_never_admissible() {
        let mut generation = opened();
        generation.republished_at(9_000);
        let replay = [8_998_u64, 8_999, 9_000, 9_001, 9_002];
        let admitted: Vec<u64> = replay
            .into_iter()
            .filter(|seq| generation.forwards(*seq))
            .collect();
        assert_eq!(
            admitted,
            vec![9_001, 9_002],
            "everything the replacement checkpoint already covers must be dropped",
        );
    }

    /// A fenced pump that is never answered ends, rather than retrying for
    /// the life of the process.
    #[tokio::test(start_paused = true)]
    async fn a_fenced_pump_gives_up_once_its_budget_is_spent() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<PaneOutput>(8);
        let mut generation = opened();
        generation.fence_for_gap();

        let mut retries = 0_u32;
        loop {
            match next_event(&generation, &mut rx).await {
                PumpWait::RetryResync => {
                    retries += 1;
                    generation.note_resync_requested();
                    assert!(retries < 32, "the fence must not retry forever");
                }
                PumpWait::GapUnrecoverable => break,
                PumpWait::Event(_) => panic!("nothing was ever broadcast"),
            }
        }
        assert_eq!(
            retries, GAP_RESYNC_MAX_ATTEMPTS,
            "one request per attempt, then the pump gives up",
        );
        drop(tx);
    }

    /// ...and an actor that *does* answer, even late, unfences the pump
    /// instead of tripping the budget.
    #[tokio::test(start_paused = true)]
    async fn a_late_resync_still_unfences_the_pump() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<PaneOutput>(8);
        let mut generation = opened();
        generation.fence_for_gap();
        generation.note_resync_requested();

        // Two backoff windows late — well past the first deadline, well
        // inside the budget.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1_600)).await;
            let _ = tx.send(PaneOutput::Resync {
                cols: 80,
                rows: 24,
                reason: crate::terminal_actor::ResyncReason::OutboundGap,
                audience: ResyncAudience::Everyone,
                base_seq: 9_000,
                bytes: bytes::Bytes::new(),
            });
        });

        loop {
            match next_event(&generation, &mut rx).await {
                PumpWait::RetryResync => generation.note_resync_requested(),
                PumpWait::Event(Ok(PaneOutput::Resync { base_seq, .. })) => {
                    generation.republished_at(base_seq);
                    break;
                }
                other => panic!(
                    "expected the late resync, got {}",
                    match other {
                        PumpWait::GapUnrecoverable => "give-up",
                        _ => "another event",
                    }
                ),
            }
        }
        assert!(!generation.is_fenced());
        assert!(generation.forwards(9_001));
    }

    /// A replacement generation returns the full budget, so a later, unrelated
    /// gap is not punished for an earlier one.
    #[test]
    fn republishing_restores_the_gap_budget() {
        let mut generation = opened();
        generation.fence_for_gap();
        for _ in 0..GAP_RESYNC_MAX_ATTEMPTS {
            generation.note_resync_requested();
        }
        assert!(generation.gap_retry_delay().is_none(), "budget spent");

        generation.republished_at(9_000);
        assert_eq!(generation.gap_attempts(), 0);
        generation.fence_for_gap();
        generation.note_resync_requested();
        assert_eq!(
            generation.gap_retry_delay(),
            Some(Duration::from_millis(500)),
            "a fresh gap starts from the first backoff step",
        );
    }
}
