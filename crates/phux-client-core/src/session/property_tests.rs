use std::fmt::{self, Write as _};

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, RngSeed, TestCaseResult};

use super::kernel_rig::{HISTORY_MAX_BYTES, HISTORY_MAX_ROWS, KernelRig, RigEvent, RigSnapshot};
use crate::history::{HistoryLoadState, ViewportAnchor};

fn unicode_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("a"),
            Just("Z"),
            Just("界"),
            Just("🙂"),
            Just("e\u{301}"),
            Just("क"),
            Just("👩\u{200d}💻"),
            Just("\n"),
        ],
        0..7,
    )
    .prop_map(|parts| parts.concat())
}

fn nonempty_unicode_text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            Just("x"),
            Just("界"),
            Just("🙂"),
            Just("e\u{301}"),
            Just("क"),
            Just("👩\u{200d}💻"),
        ],
        1..6,
    )
    .prop_map(|parts| parts.concat())
}

fn history_unavailable_reason(raw: u8) -> super::HistoryUnavailableReason {
    match raw % 8 {
        0 => super::HistoryUnavailableReason::Stale,
        1 => super::HistoryUnavailableReason::Pruned,
        2 => super::HistoryUnavailableReason::Reset,
        3 => super::HistoryUnavailableReason::Resize,
        4 => super::HistoryUnavailableReason::Expired,
        5 => super::HistoryUnavailableReason::Released,
        6 => super::HistoryUnavailableReason::Limit,
        _ => super::HistoryUnavailableReason::CodecFailure,
    }
}

fn history_rejection_reason(raw: u8) -> super::HistoryRejectionReason {
    match raw % 3 {
        0 => super::HistoryRejectionReason::ZeroLimit,
        1 => super::HistoryRejectionReason::TooSmall,
        _ => super::HistoryRejectionReason::Busy,
    }
}

#[allow(clippy::too_many_lines)]
fn operation_strategy() -> impl Strategy<Value = RigEvent> {
    prop_oneof![
        1 => any::<bool>().prop_map(|state_sync| RigEvent::Negotiate { state_sync }),
        2 => (0_u32..4).prop_map(|attach_id| RigEvent::AttachStarted { attach_id }),
        2 => (0_u32..4).prop_map(|attach_id| RigEvent::AttachReady { attach_id }),
        1 => Just(RigEvent::Disconnect),
        5 => (
            1_u8..=3,
            1_u8..=3,
            1_u16..=300,
            1_u16..=50,
            0_u64..16,
            any::<bool>(),
        )
            .prop_map(|(stream, generation, cols, rows, base_seq, exact_profile)| {
                RigEvent::Begin {
                    stream,
                    generation,
                    cols,
                    rows,
                    base_seq,
                    exact_profile,
                }
            }),
        5 => (
            1_u8..=3,
            1_u8..=3,
            1_u16..=300,
            1_u16..=50,
            0_u64..16,
            prop::option::of(0_u8..4),
            nonempty_unicode_text(),
        )
            .prop_map(
                |(stream, generation, cols, rows, base_seq, history_cursor, bootstrap_text)| {
                    RigEvent::Publish {
                        stream,
                        generation,
                        cols,
                        rows,
                        base_seq,
                        history_cursor,
                        bootstrap_text,
                    }
                },
            ),
        5 => (1_u8..=3, 1_u8..=3, 0_u32..5, unicode_text()).prop_map(
            |(stream, generation, chunk_seq, text)| RigEvent::Chunk {
                stream,
                generation,
                chunk_seq,
                text,
            },
        ),
        4 => (1_u8..=3, 1_u8..=3, prop::option::of(0_u8..4)).prop_map(
            |(stream, generation, history_cursor)| RigEvent::Ready {
                stream,
                generation,
                history_cursor,
            },
        ),
        7 => (1_u8..=3, 1_u8..=3, 0_u64..24, unicode_text()).prop_map(
            |(stream, generation, seq, text)| RigEvent::Output {
                stream,
                generation,
                seq,
                text,
            },
        ),
        4 => (1_u8..=3, 1_u8..=3, 0_u64..24, unicode_text()).prop_map(
            |(stream, generation, seq, text)| RigEvent::Resume {
                stream,
                generation,
                seq,
                text,
            },
        ),
        3 => unicode_text().prop_map(|text| RigEvent::Paste { text }),
        2 => (0_usize..10).prop_map(|rows_from_oldest| RigEvent::Prefetch {
            rows_from_oldest,
        }),
        6 => (
            1_u8..=3,
            1_u8..=3,
            0_u64..5,
            1_u32..=8,
            0_u8..4,
            prop::option::of(0_u8..4),
            nonempty_unicode_text(),
        )
            .prop_map(
                |(stream, generation, page_seq, rows, cursor, next_cursor, text)| {
                    RigEvent::HistoryPage {
                        stream,
                        generation,
                        page_seq,
                        rows,
                        cursor,
                        next_cursor,
                        text,
                    }
                },
            ),
        3 => (1_u8..=3, 1_u8..=3, 0_u8..4, any::<u8>()).prop_map(
            |(stream, generation, cursor, reason)| RigEvent::HistoryTombstone {
                stream,
                generation,
                cursor,
                reason: history_unavailable_reason(reason),
            },
        ),
        2 => (
            1_u8..=3,
            1_u8..=3,
            0_u8..4,
            any::<u8>(),
            0_u32..400,
            0_u32..40,
        )
            .prop_map(
                |(stream, generation, cursor, reason, required_bytes, required_rows)| {
                    RigEvent::HistoryRejected {
                        stream,
                        generation,
                        cursor,
                        reason: history_rejection_reason(reason),
                        required_bytes,
                        required_rows,
                    }
                },
            ),
        4 => (1_u8..=3, 1_u8..=3, 0_u64..24).prop_map(
            |(stream, generation, last_valid_seq)| RigEvent::Tombstone {
                stream,
                generation,
                last_valid_seq,
            },
        ),
        4 => (1_u16..=300, 0_usize..32).prop_map(|(width, max_rows)| {
            RigEvent::Project { width, max_rows }
        }),
        3 => (0_u16..=300, 0_u32..40).prop_map(|(x, y)| RigEvent::Track { x, y }),
        2 => (0_usize..12).prop_map(|anchor_slot| RigEvent::Pin { anchor_slot }),
        2 => Just(RigEvent::FollowTail),
        2 => (0_usize..12, 0_usize..12, any::<bool>()).prop_map(
            |(start_slot, end_slot, rectangle)| RigEvent::Select {
                start_slot,
                end_slot,
                rectangle,
            },
        ),
        1 => Just(RigEvent::InvalidateAnchors),
        1 => Just(RigEvent::Close),
    ]
}

struct Transcript<'a>(&'a [RigEvent]);

impl fmt::Display for Transcript<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut rendered = String::new();
        for (index, event) in self.0.iter().enumerate() {
            let _ = writeln!(rendered, "{index:02}: {event:?}");
        }
        formatter.write_str(&rendered)
    }
}

fn same_generation(before: &RigSnapshot, after: &RigSnapshot) -> bool {
    before
        .published
        .as_ref()
        .zip(after.published.as_ref())
        .is_some_and(|(before, after)| before.key == after.key)
}

fn assert_snapshot_bounds(after: &RigSnapshot, transcript: &Transcript<'_>) -> TestCaseResult {
    if let Some(published) = &after.published {
        prop_assert_eq!(
            published.geometry,
            published.engine_geometry,
            "kernel and engine disagree on canonical geometry\n{}",
            transcript
        );
        prop_assert!(
            published.history.retained_payload_bytes <= HISTORY_MAX_BYTES,
            "retained history exceeded byte budget: {:?}\n{}",
            published.history,
            transcript
        );
        prop_assert!(
            published.imported_history_bytes <= HISTORY_MAX_BYTES,
            "adapter retained history exceeded byte budget\n{}",
            transcript
        );
        prop_assert!(
            published.history.materialized_rows <= HISTORY_MAX_ROWS,
            "materialized history exceeded row budget: {:?}\n{}",
            published.history,
            transcript
        );
        if published.history.viewport == ViewportAnchor::Tail {
            prop_assert_eq!(
                published.history.unread_rows,
                0,
                "tail viewport accumulated unread rows\n{}",
                transcript
            );
        }
    }

    if let Some(staging) = &after.staging {
        prop_assert_eq!(
            staging.geometry,
            staging.engine_geometry,
            "staging geometry diverged from its engine\n{}",
            transcript
        );
    }
    Ok(())
}

fn assert_same_generation_invariants(
    before: &RigSnapshot,
    after: &RigSnapshot,
    event: &RigEvent,
    accepted: bool,
    transcript: &Transcript<'_>,
) -> TestCaseResult {
    if !same_generation(before, after) {
        return Ok(());
    }
    let before_replica = before
        .published
        .as_ref()
        .expect("same generation has before");
    let after_replica = after.published.as_ref().expect("same generation has after");
    prop_assert_eq!(
        before_replica.geometry,
        after_replica.geometry,
        "frontend-only operation resized canonical state\n{}",
        transcript
    );

    for (anchor, point_before) in &before_replica.anchors {
        if let Some((_, point_after)) = after_replica
            .anchors
            .iter()
            .find(|(candidate, _)| candidate == anchor)
        {
            prop_assert_eq!(
                point_before,
                point_after,
                "tracked document anchor moved instead of staying stable\n{}",
                transcript
            );
        } else {
            let history_invalidated = matches!(
                after_replica.history.state,
                HistoryLoadState::Gap
                    | HistoryLoadState::Stale
                    | HistoryLoadState::Pruned
                    | HistoryLoadState::Tombstoned
            );
            prop_assert!(
                event.explicitly_invalidates_anchors() || history_invalidated,
                "anchor disappeared without explicit invalidation\n{}",
                transcript
            );
        }
    }

    if let ViewportAnchor::Pinned(anchor_before) = before_replica.history.viewport
        && after_replica.history.viewport == ViewportAnchor::Pinned(anchor_before)
        && matches!(event, RigEvent::Output { .. } | RigEvent::Resume { .. })
        && accepted
    {
        prop_assert!(
            after_replica.history.unread_rows >= before_replica.history.unread_rows,
            "pinned viewport lost unread output accounting\n{}",
            transcript
        );
    }
    Ok(())
}

fn assert_tombstone_dominance(
    before: &RigSnapshot,
    after: &RigSnapshot,
    event: &RigEvent,
    transcript: &Transcript<'_>,
) -> TestCaseResult {
    if event.resets_connection() || matches!(event, RigEvent::Close) {
        return Ok(());
    }
    for (generation, record) in &before.tombstones {
        prop_assert_eq!(
            after
                .tombstones
                .iter()
                .find(|(candidate, _)| candidate == generation)
                .map(|(_, record)| record),
            Some(record),
            "a later event displaced an authoritative tombstone\n{}",
            transcript
        );
    }
    Ok(())
}

fn assert_resume_fence(
    before: &RigSnapshot,
    event: &RigEvent,
    accepted: bool,
    transcript: &Transcript<'_>,
) -> TestCaseResult {
    let RigEvent::Resume {
        stream,
        generation,
        seq,
        ..
    } = event
    else {
        return Ok(());
    };
    let expected = before.published.as_ref().is_some_and(|published| {
        published.key.stream_id.get() == u64::from(*stream)
            && published.key.bootstrap_id.get() == u64::from(*generation)
            && published.last_seq.checked_add(1) == Some(*seq)
            && !before
                .tombstones
                .iter()
                .any(|((retired_stream, retired_generation), _)| {
                    retired_stream == stream && retired_generation == generation
                })
    });
    prop_assert_eq!(
        accepted,
        expected,
        "resume acceptance did not match generation/sequence fence\n{}",
        transcript
    );
    Ok(())
}

fn assert_kernel_invariants(
    before: &RigSnapshot,
    after: &RigSnapshot,
    event: &RigEvent,
    accepted: bool,
    transcript: &Transcript<'_>,
) -> TestCaseResult {
    assert_snapshot_bounds(after, transcript)?;
    assert_same_generation_invariants(before, after, event, accepted, transcript)?;
    assert_tombstone_dominance(before, after, event, transcript)?;
    assert_resume_fence(before, event, accepted, transcript)
}

fn exercise(operations: &[RigEvent]) -> TestCaseResult {
    let mut rig = KernelRig::new(false);
    for (index, event) in operations.iter().enumerate() {
        let transcript = Transcript(&operations[..=index]);
        let before = rig.snapshot();
        let accepted = rig.apply(event);
        let after = rig.snapshot();
        assert_kernel_invariants(&before, &after, event, accepted, &transcript)?;

        if event.replayable_wire_event() {
            let once = after;
            let _ = rig.apply(event);
            let twice = rig.snapshot();
            prop_assert_eq!(
                once,
                twice,
                "duplicate wire event was not idempotent\n{}",
                transcript
            );
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 72,
        rng_seed: RngSeed::Fixed(0x5058_534c_4f47_4943),
        failure_persistence: None,
        max_shrink_iters: 16_384,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_session_sequences_preserve_kernel_contracts(
        operations in prop::collection::vec(operation_strategy(), 1..80),
    ) {
        exercise(&operations)?;
    }

    #[test]
    fn negotiated_profile_is_exact_and_preserves_synthesized_fallback(
        state_sync in any::<bool>(),
        cols in 1_u16..=300,
        rows in 1_u16..=50,
        base_seq in 0_u64..100,
    ) {
        let mut rig = KernelRig::new(state_sync);
        let pristine = rig.snapshot();
        let mismatched = rig.apply(&RigEvent::Begin {
            stream: 1,
            generation: 1,
            cols,
            rows,
            base_seq,
            exact_profile: false,
        });
        prop_assert!(!mismatched);
        prop_assert_eq!(rig.snapshot(), pristine);

        let matched = rig.apply(&RigEvent::Begin {
            stream: 1,
            generation: 1,
            cols,
            rows,
            base_seq,
            exact_profile: true,
        });
        prop_assert!(matched);
        let staged = rig.snapshot().staging.expect("exact synthesized profile stages");
        prop_assert_eq!(staged.geometry.cols, cols);
        prop_assert_eq!(staged.geometry.rows, rows);
        prop_assert_eq!(staged.geometry, staged.engine_geometry);
    }

    #[test]
    fn resume_requires_exact_published_generation_and_fence(
        state_sync in any::<bool>(),
        cols in 1_u16..=300,
        rows in 1_u16..=50,
        base_seq in 0_u64..100,
        bootstrap_text in nonempty_unicode_text(),
        live_text in unicode_text(),
    ) {
        let mut rig = KernelRig::new(state_sync);
        let begin = RigEvent::Begin {
            stream: 1,
            generation: 1,
            cols,
            rows,
            base_seq,
            exact_profile: true,
        };
        prop_assert!(rig.apply(&begin));

        let premature = RigEvent::Resume {
            stream: 1,
            generation: 1,
            seq: base_seq.saturating_add(1),
            text: live_text.clone(),
        };
        prop_assert!(!rig.apply(&premature));
        prop_assert!(rig.snapshot().published.is_none());

        let chunked = rig.apply(&RigEvent::Chunk {
            stream: 1,
            generation: 1,
            chunk_seq: 0,
            text: bootstrap_text,
        });
        prop_assert!(chunked);
        let ready = rig.apply(&RigEvent::Ready {
            stream: 1,
            generation: 1,
            history_cursor: None,
        });
        prop_assert!(ready);
        let published = rig.snapshot();
        let geometry = published.published.as_ref().expect("dual-ready publishes").geometry;
        prop_assert_eq!(geometry.cols, cols);
        prop_assert_eq!(geometry.rows, rows);
        let checkpoint = rig.resume_checkpoint().expect("published replica is resumable");
        prop_assert_eq!(checkpoint.stream, 1);
        prop_assert_eq!(checkpoint.generation, 1);
        prop_assert_eq!(checkpoint.next_seq, base_seq.saturating_add(1));

        let stale_stream = RigEvent::Resume {
            stream: checkpoint.stream.saturating_add(1),
            generation: checkpoint.generation,
            seq: checkpoint.next_seq,
            text: live_text.clone(),
        };
        let stale_generation = RigEvent::Resume {
            stream: checkpoint.stream,
            generation: checkpoint.generation.saturating_add(1),
            seq: checkpoint.next_seq,
            text: live_text.clone(),
        };
        let stale_fence = RigEvent::Resume {
            stream: checkpoint.stream,
            generation: checkpoint.generation,
            seq: checkpoint.next_seq.saturating_add(1),
            text: live_text.clone(),
        };
        prop_assert!(!rig.apply(&stale_stream));
        prop_assert!(!rig.apply(&stale_generation));
        prop_assert!(!rig.apply(&stale_fence));
        prop_assert_eq!(rig.snapshot(), published);

        let matching = RigEvent::Resume {
            stream: checkpoint.stream,
            generation: checkpoint.generation,
            seq: checkpoint.next_seq,
            text: live_text,
        };
        prop_assert!(rig.apply(&matching));
        prop_assert_eq!(
            rig.snapshot().published.as_ref().expect("still published").last_seq,
            checkpoint.next_seq,
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn history_projection_pruning_and_selection_preserve_live_geometry(
        cols in 1_u16..=300,
        projection_width in 1_u16..=300,
        pages in prop::collection::vec(nonempty_unicode_text(), 1..18),
        live_text in nonempty_unicode_text(),
    ) {
        let mut rig = KernelRig::new(false);
        let published = rig.apply(&RigEvent::Publish {
            stream: 1,
            generation: 1,
            cols,
            rows: 24,
            base_seq: 10,
            history_cursor: Some(0),
            bootstrap_text: "boot界e\u{301}".to_owned(),
        });
        prop_assert!(published);
        let paste = "界e\u{301}👩\u{200d}💻";
        let pasted = rig.apply(&RigEvent::Paste {
            text: paste.to_owned(),
        });
        prop_assert!(pasted);
        prop_assert_eq!(rig.last_paste_payload(), Some(paste.as_bytes()));
        let tracked = rig.apply(&RigEvent::Track { x: 0, y: 0 });
        prop_assert!(tracked);
        let pinned_viewport = rig.apply(&RigEvent::Pin { anchor_slot: 0 });
        prop_assert!(pinned_viewport);
        let pinned = rig.snapshot();
        let anchor = pinned
            .published
            .as_ref()
            .expect("published")
            .anchors
            .first()
            .copied()
            .expect("tracked anchor");

        for (index, text) in pages.iter().enumerate() {
            let cursor = u8::try_from(index).unwrap_or(u8::MAX);
            let next_cursor = Some(cursor.saturating_add(1));
            let _ = rig.apply(&RigEvent::HistoryPage {
                stream: 1,
                generation: 1,
                page_seq: 1,
                rows: 1,
                cursor,
                next_cursor,
                text: text.clone(),
            });
            let snapshot = rig.snapshot();
            let published = snapshot.published.as_ref().expect("history keeps live replica");
            prop_assert!(published.history.retained_payload_bytes <= HISTORY_MAX_BYTES);
            prop_assert!(published.history.materialized_rows <= HISTORY_MAX_ROWS);
        }
        let retained = rig.snapshot();
        let loaded = &retained
            .published
            .as_ref()
            .expect("history remains published")
            .history;
        if pages.iter().map(String::len).sum::<usize>() > HISTORY_MAX_BYTES {
            prop_assert!(
                loaded.loaded_pages < pages.len(),
                "over-budget history did not prune an opaque page"
            );
        }

        let projected_history = rig.apply(&RigEvent::Project {
            width: projection_width,
            max_rows: 32,
        });
        prop_assert!(projected_history);
        let projected = rig.snapshot();
        let published = projected.published.as_ref().expect("projected replica");
        prop_assert_eq!(published.geometry.cols, cols);
        prop_assert_eq!(published.engine_geometry.cols, cols);
        prop_assert_eq!(published.history.projection_width, projection_width.max(2));
        prop_assert_eq!(published.anchors.first().copied(), Some(anchor));

        let selected_document = rig.apply(&RigEvent::Select {
            start_slot: 0,
            end_slot: 0,
            rectangle: false,
        });
        prop_assert!(selected_document);
        let selected = rig.selection_text(0, 0).expect("valid anchors select text");
        prop_assert!(
            selected.contains(
                pages
                    .last()
                    .expect("generated at least one page")
                    .as_str()
            )
        );
        let applied_output = rig.apply(&RigEvent::Output {
            stream: 1,
            generation: 1,
            seq: 11,
            text: live_text,
        });
        prop_assert!(applied_output);
        let scrolled = rig.snapshot();
        let published = scrolled.published.as_ref().expect("output keeps replica");
        prop_assert!(published.history.unread_rows >= 1);
        prop_assert_eq!(published.history.viewport, ViewportAnchor::Pinned(anchor.0));
        prop_assert_eq!(published.anchors.first().copied(), Some(anchor));

        prop_assert!(rig.apply(&RigEvent::FollowTail));
        let tail = rig.snapshot();
        let history = &tail.published.as_ref().expect("tail replica").history;
        prop_assert_eq!(history.viewport, ViewportAnchor::Tail);
        prop_assert_eq!(history.unread_rows, 0);

        let repinned = rig.apply(&RigEvent::Pin { anchor_slot: 0 });
        prop_assert!(repinned);
        prop_assert!(rig.apply(&RigEvent::InvalidateAnchors));
        let invalidated = rig.snapshot();
        let history = &invalidated.published.as_ref().expect("invalidated replica").history;
        prop_assert_eq!(history.state, HistoryLoadState::Pruned);
        prop_assert_eq!(history.viewport, ViewportAnchor::Tail);
    }

    #[test]
    fn first_tombstone_dominates_all_late_generation_events(
        cols in 1_u16..=300,
        base_seq in 0_u64..100,
        late_text in unicode_text(),
        first_last_valid in 0_u64..100,
        later_last_valid in 101_u64..200,
    ) {
        let mut rig = KernelRig::new(false);
        let published = rig.apply(&RigEvent::Publish {
            stream: 1,
            generation: 1,
            cols,
            rows: 24,
            base_seq,
            history_cursor: None,
            bootstrap_text: "ready".to_owned(),
        });
        prop_assert!(published);
        let tombstoned = rig.apply(&RigEvent::Tombstone {
            stream: 1,
            generation: 1,
            last_valid_seq: first_last_valid,
        });
        prop_assert!(tombstoned);
        prop_assert!(rig.resume_checkpoint().is_none());
        let authoritative = rig.snapshot();
        let first = authoritative.tombstones.clone();

        let late_output = rig.apply(&RigEvent::Output {
            stream: 1,
            generation: 1,
            seq: base_seq.saturating_add(1),
            text: late_text,
        });
        prop_assert!(!late_output);
        let late_begin = rig.apply(&RigEvent::Begin {
            stream: 1,
            generation: 1,
            cols,
            rows: 24,
            base_seq,
            exact_profile: true,
        });
        prop_assert!(!late_begin);
        let repeated_tombstone = rig.apply(&RigEvent::Tombstone {
            stream: 1,
            generation: 1,
            last_valid_seq: later_last_valid,
        });
        prop_assert!(repeated_tombstone);
        let after = rig.snapshot();
        prop_assert_eq!(after.tombstones, first);
        prop_assert_eq!(
            after
                .published
                .as_ref()
                .expect("tombstone freezes last view")
                .live
                .as_slice(),
            authoritative
                .published
                .as_ref()
                .expect("authoritative view")
                .live
                .as_slice(),
        );
    }
}
