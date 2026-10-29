use super::*;

/// Seeding fires `after-new-pane`, and the exit watcher fires
/// `pane-exit` with the exit code (docs/hooks.md).
#[test]
fn pane_lifecycle_hooks_fire() {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let state = SharedState::new();
        let mut hooks = capture_hooks(&state);
        let token = CancellationToken::new();
        seed_session_with_actor(
            &state,
            "hooked",
            phux_config::ScrollbackLimits::new(100, phux_config::DEFAULT_HISTORY_BYTES),
            &token,
        )
        .expect("seed");
        let event = hooks.try_recv().expect("after-new-pane fired");
        assert_eq!(event.name, crate::hooks::AFTER_NEW_PANE);
        assert_eq!(
            event.context.get("session").map(String::as_str),
            Some("hooked")
        );
        assert!(event.context.contains_key("terminal-id"));

        let (_sid, pane) = state.with_mut(|s| s.seed_session("dying"));
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        spawn_terminal_exit_watcher(state.clone(), pane, Some(exit_rx), token, None);
        exit_tx
            .send(phux_core::process::ExitOutcome::exited(3))
            .expect("exit notify");
        let event = tokio::time::timeout(MAILBOX_DEADLINE, hooks.recv())
            .await
            .expect("pane-exit hook timed out")
            .expect("hook channel closed");
        assert_eq!(event.name, crate::hooks::PANE_EXIT);
        assert_eq!(
            event.context.get("exit-code").map(String::as_str),
            Some("3")
        );
        assert!(event.context.contains_key("terminal-id"));
    });
}

/// A kill reaps its pane in its own commit (L1 §5.2), before the process
/// ends its hangup grace: the watcher still fires `pane-exit` with the
/// status the process ended with, and last-session self-exit waits for
/// that process instead of cutting its grace short.
#[test]
fn a_pane_killed_before_its_exit_still_fires_pane_exit_and_holds_self_exit() {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let state = SharedState::new();
        let mut hooks = capture_hooks(&state);
        let root = CancellationToken::new();
        let (_sid, pane) = state.with_mut(|s| s.seed_session("doomed"));
        state.with_mut(crate::state::ServerState::arm_self_exit);
        let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
        spawn_terminal_exit_watcher(state.clone(), pane, Some(exit_rx), root.clone(), None);

        let committed = state.with_mut(|s| {
            committed_close::commit_close(
                s,
                &[pane],
                phux_protocol::wire::frame::CloseReason::Killed,
                crate::state::CloseAttribution::default(),
            )
        });
        assert_eq!(committed.len(), 1);
        committed_close::announce(&state, committed);
        assert!(
            state.with(|s| s.registry().record(pane).is_none() && !s.self_exit_due()),
            "the record is gone at commit, but the process is still in its grace"
        );

        exit_tx
            .send(phux_core::process::ExitOutcome::exited(7))
            .expect("exit notify");
        let event = tokio::time::timeout(MAILBOX_DEADLINE, hooks.recv())
            .await
            .expect("pane-exit hook timed out")
            .expect("hook channel closed");
        assert_eq!(event.name, crate::hooks::PANE_EXIT);
        assert_eq!(
            event.context.get("exit-code").map(String::as_str),
            Some("7")
        );
        tokio::time::timeout(MAILBOX_DEADLINE, root.cancelled())
            .await
            .expect("the last watcher's end self-exits");
    });
}

/// A client's emulator reply never reaches the PTY (the canonical
/// terminal already answered), and `focus-changed` fires only for an
/// authorized focus-gained event.
#[test]
fn terminal_replies_are_discarded_and_focus_hooks_fire() {
    use phux_protocol::input::focus::FocusEvent;

    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let state = SharedState::new();
        let mut hooks = capture_hooks(&state);
        let (_sid, pane) = state.with_mut(|s| s.seed_session("focus"));
        let mut rx = register_stub(&state, pane);
        let wire_terminal_id = state.with_mut(|s| s.intern_terminal_wire(pane));
        let client_id = attach_client(&state, "focus");

        let reply = bytes::Bytes::from_static(b"\x1b[5;1R");
        handle_terminal_reply(client_id, &wire_terminal_id, &reply);
        assert!(
            rx.encoded_input.try_recv().is_err(),
            "reply must be discarded"
        );

        let focus = |client, event| {
            handle_terminal_input(
                &state,
                client,
                &wire_terminal_id,
                crate::state::TerminalInput::Focus(event),
                "INPUT_FOCUS",
            );
        };
        focus(client_id, FocusEvent::Gained);
        let event = hooks.try_recv().expect("focus-changed fired");
        assert_eq!(event.name, crate::hooks::FOCUS_CHANGED);
        assert!(event.context.contains_key("terminal-id"));
        assert!(event.context.contains_key("client-id"));

        focus(client_id, FocusEvent::Lost);
        assert!(hooks.try_recv().is_err(), "focus lost fires nothing");
        focus(ClientId(4242), FocusEvent::Gained);
        assert!(
            hooks.try_recv().is_err(),
            "a gated focus frame fires nothing"
        );
    });
}

/// Teardown fires `client-detached` with the session name, but only for
/// a client that attached.
#[test]
fn detach_fires_client_detached_hook_only_for_attached_clients() {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    let local = LocalSet::new();
    local.block_on(&rt, async {
        let state = SharedState::new();
        let mut hooks = capture_hooks(&state);
        let _ = state.with_mut(|s| s.seed_session("leaving"));

        let stranger = state.with_mut(crate::state::ServerState::new_client_id);
        detach_and_release_consumer_state(&state, stranger);
        assert!(hooks.try_recv().is_err());

        let client_id = attach_client(&state, "leaving");
        detach_and_release_consumer_state(&state, client_id);
        let event = hooks.try_recv().expect("client-detached fired");
        assert_eq!(event.name, crate::hooks::CLIENT_DETACHED);
        assert_eq!(
            event.context.get("session").map(String::as_str),
            Some("leaving")
        );
    });
}
