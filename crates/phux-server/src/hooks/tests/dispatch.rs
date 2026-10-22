use super::*;

#[test]
fn fire_on_full_or_closed_queue_never_blocks_or_panics() {
    // Full queue: capacity 1, nothing draining. The second fire drops.
    let (tx, rx) = mpsc::channel::<HookEvent>(1);
    let dispatcher = HookDispatcher::from_sender(tx);
    let started = Instant::now();
    dispatcher.fire(HookEvent::new(PANE_EXIT, []));
    dispatcher.fire(HookEvent::new(PANE_EXIT, []));
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "fire must be non-blocking even when the queue is full",
    );
    // Closed queue: receiver gone. Fire is a logged no-op.
    drop(rx);
    dispatcher.fire(HookEvent::new(PANE_EXIT, []));
}

#[test]
fn fire_hook_without_registered_dispatcher_is_noop() {
    let state = crate::state::SharedState::new();
    fire_hook(&state, HookEvent::new(PANE_EXIT, []));
}

/// A config `run` hook executes with the event env and `PHUX_SOCKET`.
#[tokio::test(flavor = "current_thread")]
async fn dispatcher_executes_config_hook_with_env_injection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let socket = dir.path().join("phux.sock");
    let command = format!(
        "printf '%s %s %s' \"$PHUX_EVENT\" \"$PHUX_TERMINAL_ID\" \"$PHUX_SOCKET\" > {}",
        marker.display()
    );
    let entry = HookEntry {
        when: BTreeMap::new(),
        action: Action::Parameterized(phux_config::ParamAction {
            action: "run".to_owned(),
            args: std::iter::once(("command".to_owned(), toml::Value::String(command))).collect(),
        }),
    };
    let catalog = HookCatalog {
        config_hooks: std::iter::once((AFTER_NEW_PANE.to_owned(), vec![entry])).collect(),
        plugin_events: Vec::new(),
    };

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dispatcher = spawn_hook_dispatcher(catalog, Some(socket.clone()), None);
            dispatcher.fire(HookEvent::new(
                AFTER_NEW_PANE,
                [("terminal-id".to_owned(), "42".to_owned())],
            ));
            wait_for_file(&marker).await;
        })
        .await;
    let contents = std::fs::read_to_string(&marker).expect("marker written");
    assert_eq!(contents, format!("after-new-pane 42 {}", socket.display()));
}

/// A plugin hook runs in the plugin root with its identity env, and its
/// run lands in the run log.
#[tokio::test(flavor = "current_thread")]
async fn dispatcher_executes_plugin_event_in_plugin_root_with_plugin_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().canonicalize().expect("canonical root");
    let catalog = HookCatalog {
        config_hooks: BTreeMap::new(),
        plugin_events: vec![PluginEventHook {
            plugin_id: "notifier".to_owned(),
            event_id: "on-exit".to_owned(),
            on: PANE_EXIT.to_owned(),
            command: vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf '%s %s %s' \"$PHUX_PLUGIN_ID\" \"$PHUX_EXIT_CODE\" \"$PWD\" > marker"
                    .to_owned(),
            ],
            plugin_root: root.clone(),
        }],
    };
    let marker = root.join("marker");
    let run_log = root.join("runs.jsonl");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dispatcher = spawn_hook_dispatcher(catalog, None, Some(run_log.clone()));
            dispatcher.fire(HookEvent::new(
                PANE_EXIT,
                [("exit-code".to_owned(), "0".to_owned())],
            ));
            wait_for_file(&marker).await;
            wait_for_file(&run_log).await;
        })
        .await;
    let contents = std::fs::read_to_string(&marker).expect("marker written");
    assert_eq!(contents, format!("notifier 0 {}", root.display()));
    let records = phux_plugin::run_log::read(&run_log).expect("run log reads");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].kind, phux_plugin::run_log::RunKind::Hook);
    assert_eq!(records[0].plugin_id.as_deref(), Some("notifier"));
    assert_eq!(records[0].name, "plugin.notifier.on-exit");
    assert!(records[0].succeeded(), "{records:?}");
}

/// A slow hook does not block `fire`.
#[tokio::test(flavor = "current_thread")]
async fn fire_returns_immediately_while_hook_still_runs() {
    let catalog = catalog_from_toml(
        r#"
        [[hooks.pane-exit]]
        action = { kind = "run", command = "sleep 30" }
        "#,
    );
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dispatcher = spawn_hook_dispatcher(catalog, None, None);
            let started = Instant::now();
            dispatcher.fire(HookEvent::new(PANE_EXIT, []));
            assert!(
                started.elapsed() < Duration::from_millis(200),
                "fire must not wait for the hook child",
            );
            // Let it spawn, then drop everything (kill_on_drop reaps).
            tokio::time::sleep(Duration::from_millis(10)).await;
        })
        .await;
}

/// Poll for `path` to appear (the hook child writes it asynchronously).
async fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        assert!(Instant::now() < deadline, "hook never wrote {path:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // A final beat so the write is complete, not just the file created.
    tokio::time::sleep(Duration::from_millis(20)).await;
}
