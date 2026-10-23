use super::*;

/// Superseded expiry timers are aborted, not left sleeping: repeated
/// re-arms once leaked one live task per grant.
#[tokio::test(flavor = "current_thread")]
async fn rearming_a_lease_repeatedly_leaves_at_most_one_live_timer_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let state = SharedState::new();
            let core = state.with_mut(|s| s.seed_session("default")).1;
            let counter = state.with(crate::state::ServerState::input_lease_expiry_task_counter);

            for _ in 0..20 {
                let generation = state
                    .with_mut(|s| s.refresh_input_lease_expiry(core, true))
                    .expect("scheduled");
                spawn_input_lease_expiry(&state, core, generation, 60_000);
                // Let the new task start and the aborted one unwind.
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
            }

            assert!(
                counter.load(std::sync::atomic::Ordering::SeqCst) <= 1,
                "20 re-arms must not leave more than one live timer task, got {}",
                counter.load(std::sync::atomic::Ordering::SeqCst)
            );
        })
        .await;
}
