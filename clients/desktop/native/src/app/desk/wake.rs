//! Keep a continuously refilled runtime wake channel from owning the UI thread.

use std::time::Duration;

/// A wake is only a hint: the runtime retains the events and terminal bytes.
/// Drain immediately after idle. If another hint is already queued, return to
/// the platform event loop before draining it: awaiting a ready `recv` does
/// not yield. An empty channel already provides that boundary; do not add a
/// timer round trip to normal isolated updates. The bounded channel coalesces
/// arrivals during a pause; no output is dropped.
pub(super) async fn pump(
    rx: async_channel::Receiver<()>,
    timer: gpui::BackgroundExecutor,
    mut drain: impl FnMut() -> bool,
) {
    while rx.recv().await.is_ok() {
        if !drain() {
            break;
        }
        if !rx.is_empty() {
            timer.timer(Duration::from_millis(1)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn saturated_wakes_yield_before_the_next_drain(cx: &mut gpui::TestAppContext) {
        let (tx, rx) = async_channel::bounded(1);
        tx.try_send(()).unwrap();
        let count = Rc::new(Cell::new(0));
        let drained = Rc::clone(&count);
        let task = cx.update(|app| {
            let timer = app.background_executor().clone();
            app.spawn(async move |_| {
                pump(rx, timer, || {
                    let next = drained.get() + 1;
                    drained.set(next);
                    if next < 3 {
                        // Reproduce an output thread refilling the wake while
                        // the foreground drains it. recv() will be ready again.
                        tx.try_send(()).unwrap();
                    }
                    true
                })
                .await;
            })
        });
        assert!(cx.executor().tick());
        assert_eq!(
            count.get(),
            1,
            "one poll must not drain a hot channel twice"
        );
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(count.get(), 3, "pending wakes must not be lost");
        drop(task);
    }

    #[gpui::test]
    fn idle_wake_drains_immediately_and_coalesced_work_survives(cx: &mut gpui::TestAppContext) {
        let (tx, rx) = async_channel::bounded(1);
        let count = Rc::new(Cell::new(0));
        let drained = Rc::clone(&count);
        let task = cx.update(|app| {
            let timer = app.background_executor().clone();
            app.spawn(async move |_| {
                pump(rx, timer, || {
                    drained.set(drained.get() + 1);
                    true
                })
                .await;
            })
        });
        cx.run_until_parked();
        assert_eq!(count.get(), 0, "idle must not poll or redraw");
        tx.try_send(()).unwrap();
        assert!(cx.executor().tick());
        assert_eq!(count.get(), 1, "first wake has no timer latency");
        tx.try_send(()).unwrap();
        assert!(tx.try_send(()).is_err(), "wake channel remains bounded");
        cx.run_until_parked();
        assert_eq!(count.get(), 2, "an isolated wake must not wait for a timer");
        drop(tx);
        cx.run_until_parked();
        assert_eq!(count.get(), 2, "closed channel must stop, not spin");
        drop(task);
    }

    #[gpui::test]
    fn lost_desk_stops_even_with_another_wake_queued(cx: &mut gpui::TestAppContext) {
        let (tx, rx) = async_channel::bounded(1);
        tx.try_send(()).unwrap();
        let count = Rc::new(Cell::new(0));
        let drained = Rc::clone(&count);
        let task = cx.update(|app| {
            let timer = app.background_executor().clone();
            app.spawn(async move |_| {
                pump(rx, timer, || {
                    drained.set(drained.get() + 1);
                    tx.try_send(()).unwrap();
                    false
                })
                .await;
            })
        });
        cx.run_until_parked();
        assert_eq!(count.get(), 1, "a closed window cannot be drained again");
        drop(task);
    }
}
