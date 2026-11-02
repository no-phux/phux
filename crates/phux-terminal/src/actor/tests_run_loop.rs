use super::{
    Bytes, DEFAULT_TICK_INTERVAL, OwedResync, PtyEvent, RESIZE_RESYNC_DEBOUNCE, ResyncAudience,
    ResyncDebounce, ResyncReason, ResyncTarget, TerminalActor, armed_interval,
    icount_join_pty_burst, join_pty_burst,
};

#[cfg(test)]
mod tick_rearm_tests {
    use super::{DEFAULT_TICK_INTERVAL, armed_interval};

    /// Re-arming a long-disarmed tick owes exactly one tick (`Delay`), not
    /// every missed period.
    #[tokio::test(start_paused = true)]
    async fn disarming_the_tick_does_not_bank_a_stampede_of_catch_up_ticks() {
        let mut tick = armed_interval(DEFAULT_TICK_INTERVAL).await;

        // An hour with the arm's precondition false: nothing polls the timer.
        tokio::time::advance(std::time::Duration::from_secs(3600)).await;

        // Count ticks ready without the paused clock advancing.
        let mut immediate = 0_u32;
        while tokio::time::timeout(std::time::Duration::ZERO, tick.tick())
            .await
            .is_ok()
        {
            immediate += 1;
            // Bail out rather than counting to 120,000 on a regression.
            if immediate > 8 {
                break;
            }
        }

        assert_eq!(
            immediate, 1,
            "re-arming a long-disarmed tick must yield one catch-up tick, not one per \
             missed period",
        );
    }
}

#[cfg(test)]
mod input_snapshot_fast_path_tests {
    use std::time::Instant;

    use super::{Bytes, PtyEvent, TerminalActor};

    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    async fn feed(actor: &mut TerminalActor, bytes: &'static [u8]) {
        actor
            .service_pty_event(Some(PtyEvent::Bytes {
                chunk: Bytes::from_static(bytes),
                read_at: Instant::now(),
            }))
            .await;
    }

    /// Only a mode change publishes the snapshot: plain echo does not, and a
    /// CSI split across chunks publishes on the chunk that completes it.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: TerminalActor owns !Send Terminal; lives on LocalSet"
    )]
    #[tokio::test]
    async fn only_a_mode_change_publishes_the_snapshot() {
        let bundle = TerminalActor::new(80, 24).expect("actor");
        let mut actor = bundle.actor;
        let mut snapshots = bundle
            .handle
            .facet::<crate::TerminalHandle>()
            .expect("terminal facet")
            .input_snapshot
            .clone();
        snapshots.borrow_and_update();

        feed(&mut actor, b"hello").await;
        assert!(
            !snapshots.has_changed().expect("publisher alive"),
            "plain text does not change encoder modes"
        );

        feed(&mut actor, b"\x1b[?2004h").await;
        assert!(snapshots.has_changed().expect("publisher alive"));
        assert!(snapshots.borrow_and_update().bracketed_paste);

        feed(&mut actor, b"a").await;
        assert!(!snapshots.has_changed().expect("publisher alive"));

        // An open sequence has changed nothing yet.
        feed(&mut actor, b"\x1b").await;
        assert!(!snapshots.has_changed().expect("publisher alive"));
        assert!(snapshots.borrow().bracketed_paste);

        feed(&mut actor, b"[?2004l").await;
        assert!(snapshots.has_changed().expect("publisher alive"));
        assert!(!snapshots.borrow_and_update().bracketed_paste);

        feed(&mut actor, b"z").await;
        assert!(!snapshots.has_changed().expect("publisher alive"));
    }
}

#[cfg(test)]
mod pty_burst_tests {
    use super::{Bytes, join_pty_burst};

    /// A lone chunk is forwarded without a copy; a burst joins in order into
    /// one buffer.
    #[test]
    fn a_burst_joins_in_order_and_a_lone_chunk_is_not_copied() {
        let first = Bytes::from_static(b"abc");
        let lone = join_pty_burst(first.clone(), &[], first.len());
        assert_eq!(lone.as_ptr(), first.as_ptr(), "a lone chunk moves through");

        let rest = [Bytes::from_static(b"de"), Bytes::from_static(b"fghi")];
        let joined = join_pty_burst(first, &rest, 9);
        assert_eq!(&joined[..], b"abcdefghi");
    }
}

#[cfg(test)]
mod resync_debounce_tests {
    use std::time::Duration;

    use super::{
        OwedResync, RESIZE_RESYNC_DEBOUNCE, ResyncAudience, ResyncDebounce, ResyncReason,
        ResyncTarget,
    };

    fn idle() -> ResyncDebounce {
        ResyncDebounce::idle()
    }

    fn pump(owner: u64) -> ResyncTarget {
        ResyncTarget {
            owner,
            stream_id: phux_protocol::ids::StreamId::new(1).expect("non-zero stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("non-zero bootstrap id"),
        }
    }

    /// A gap resync owed to one pump.
    const fn gap_for(target: ResyncTarget) -> OwedResync {
        OwedResync {
            reason: ResyncReason::OutboundGap,
            target: Some(target),
        }
    }

    /// A gap resync that named no pump: owed to everyone.
    const fn gap() -> OwedResync {
        OwedResync {
            reason: ResyncReason::OutboundGap,
            target: None,
        }
    }

    const fn resize() -> OwedResync {
        OwedResync {
            reason: ResyncReason::Resize,
            target: None,
        }
    }

    /// Many lagged pumps retrying must not keep pushing the debounce out:
    /// the deadline is fixed by the first gap request.
    #[tokio::test(start_paused = true)]
    async fn a_pending_gap_resync_is_not_pushed_out_by_more_lagged_consumers() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(0)), sleep.as_mut());
        let first_deadline = sleep.deadline();
        assert!(debounce.pending);

        for round in 0..20_u64 {
            // Faster than the debounce, which is exactly the starving case.
            tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
            debounce.arm(gap_for(pump(round % 10)), sleep.as_mut());
            assert_eq!(
                sleep.deadline(),
                first_deadline,
                "a gap resync already owed must coalesce onto the pending deadline, \
                 not restart it",
            );
        }

        // And it really does come due: the deadline is in the past by now.
        assert!(
            sleep.deadline() <= tokio::time::Instant::now(),
            "the coalesced snapshot must have become due despite the request storm",
        );
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        let expected: Vec<_> = (0..10).map(pump).collect();
        assert_eq!(
            audience,
            ResyncAudience::Only(expected.into()),
            "every lagged pump rides the one snapshot, each named once",
        );
    }

    /// EOF folds queued gap requests in without moving the deadline.
    #[tokio::test(start_paused = true)]
    async fn eof_flush_includes_queued_gap_requests_without_rearming_the_deadline() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let first_deadline = sleep.deadline();
        debounce.include(gap_for(pump(2)));
        assert_eq!(
            sleep.deadline(),
            first_deadline,
            "including a queued gap request at EOF must not wait another debounce",
        );
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        assert_eq!(
            audience,
            ResyncAudience::Only(vec![pump(1), pump(2)].into())
        );
    }

    /// A gap resync is owed only to the pumps that asked, and the next
    /// window starts empty.
    #[tokio::test(start_paused = true)]
    async fn a_gap_resync_is_addressed_only_to_the_pumps_that_asked() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(gap_for(pump(2)), sleep.as_mut());
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let (reason, audience) = debounce.take();
        assert_eq!(reason, ResyncReason::OutboundGap);
        assert_eq!(
            audience,
            ResyncAudience::Only(vec![pump(1), pump(2)].into())
        );
        assert!(!audience.includes(pump(3)), "a fresh pump is not addressed");

        debounce.arm(gap_for(pump(3)), sleep.as_mut());
        assert_eq!(
            debounce.take().1,
            ResyncAudience::Only(vec![pump(3)].into())
        );
    }

    /// A resize in the same window widens a gap resync to everyone, in either
    /// order; an untargeted gap request is everyone.
    #[tokio::test(start_paused = true)]
    async fn a_resize_or_an_unnamed_gap_widens_the_audience_to_everyone() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);

        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(resize(), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::Resize, ResyncAudience::Everyone)
        );

        let mut debounce = idle();
        debounce.arm(resize(), sleep.as_mut());
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::Resize, ResyncAudience::Everyone)
        );

        let mut debounce = idle();
        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        debounce.arm(gap(), sleep.as_mut());
        assert_eq!(
            debounce.take(),
            (ResyncReason::OutboundGap, ResyncAudience::Everyone)
        );
    }

    /// A resize storm still re-arms every time.
    #[tokio::test(start_paused = true)]
    async fn a_resize_still_restarts_the_debounce() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(resize(), sleep.as_mut());
        let first_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(resize(), sleep.as_mut());
        assert!(
            sleep.deadline() > first_deadline,
            "a drag storm must settle on the last size",
        );
    }

    /// A resize arriving while a gap resync is owed re-arms with its reason.
    #[tokio::test(start_paused = true)]
    async fn a_resize_supersedes_a_pending_gap_resync() {
        let sleep = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(sleep);
        let mut debounce = idle();

        debounce.arm(gap_for(pump(1)), sleep.as_mut());
        let gap_deadline = sleep.deadline();
        tokio::time::advance(RESIZE_RESYNC_DEBOUNCE / 4).await;
        debounce.arm(resize(), sleep.as_mut());

        assert!(sleep.deadline() > gap_deadline);
        assert_eq!(debounce.take().0, ResyncReason::Resize);
    }
}

#[cfg(test)]
mod icount_join_tests {
    use super::icount_join_pty_burst;
    use bytes::Bytes;

    #[test]
    fn a_burst_is_one_payload() {
        let joined =
            icount_join_pty_burst(vec![Bytes::from_static(b"ab"), Bytes::from_static(b"cd")]);
        assert_eq!(joined.as_ref(), b"abcd");
    }

    #[test]
    fn a_lone_chunk_is_moved() {
        let joined = icount_join_pty_burst(vec![Bytes::from_static(b"only")]);
        assert_eq!(joined.as_ref(), b"only");
    }
}
