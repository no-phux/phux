use super::Bytes;

/// Join already-read PTY chunks the way one actor burst does.
///
/// The instruction-count guard measures the copy. The actor's coalesce loop
/// is the production caller of [`join_pty_burst`].
pub(in crate::resource::terminal) fn icount_join_pty_burst(chunks: Vec<Bytes>) -> Bytes {
    let mut iter = chunks.into_iter();
    let Some(first) = iter.next() else {
        return Bytes::new();
    };
    let rest: Vec<Bytes> = iter.collect();
    let total = first.len() + rest.iter().map(Bytes::len).sum::<usize>();
    join_pty_burst(first, &rest, total)
}

/// Join one burst into a single payload. A lone chunk moves through as-is;
/// a burst (every burst on macOS, where reads cap at 1 KiB) is copied once
/// into a buffer sized to exactly `total` bytes, so there is no regrowth and
/// the broadcast ring retains no spare capacity.
pub(super) fn join_pty_burst(first: Bytes, rest: &[Bytes], total: usize) -> Bytes {
    if rest.is_empty() {
        return first;
    }
    let mut joined = Vec::with_capacity(total);
    joined.extend_from_slice(&first);
    for chunk in rest {
        joined.extend_from_slice(chunk);
    }
    debug_assert_eq!(joined.len(), total, "burst total was exact");
    Bytes::from(joined)
}

/// A `MissedTickBehavior::Delay` interval with its immediate first tick
/// consumed. `Delay` spaces late ticks instead of bursting to catch up.
pub(super) async fn armed_interval(period: std::time::Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = interval.tick().await;
    interval
}
