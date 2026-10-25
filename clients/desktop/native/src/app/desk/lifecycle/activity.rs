//! Coalesced connection wake delivery, with foreground executor fairness.

use std::time::Duration;

/// A full wake channel must not keep the foreground executor inside one poll.
/// Yield only when the next wake is already queued: ordinary request/response
/// activity must not wait for a timer. The channel coalesces arrivals during
/// the pause, while the runtime retains the events until the next drain.
pub(super) async fn pump_activity(
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
mod wake_tests {
    use super::pump_activity;
    use std::{cell::Cell, rc::Rc, time::Duration};

    #[gpui::test]
    fn a_full_wake_channel_gives_other_foreground_work_a_turn(cx: &mut gpui::TestAppContext) {
        let (tx, rx) = async_channel::bounded(1);
        tx.try_send(()).unwrap();
        let count = Rc::new(Cell::new(0));
        let drained = Rc::clone(&count);
        let task = cx.update(|app| {
            let timer = app.background_executor().clone();
            app.spawn(async move |_| {
                pump_activity(rx, timer, || {
                    let next = drained.get() + 1;
                    drained.set(next);
                    if next < 3 {
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
            "a ready second wake must not monopolize one poll"
        );
        for next in 2..=3 {
            cx.executor().advance_clock(Duration::from_millis(1));
            cx.run_until_parked();
            assert_eq!(count.get(), next, "coalesced wakes must still drain");
        }
        drop(task);
    }
}
