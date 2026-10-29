/// `phux_wait` — poll the pane named by the selector until a condition holds.
async fn phux_wait(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_or_default(args);
    let selector = parse_target(args)?;
    // `until` wins; otherwise settle on idle, as `phux wait`.
    let condition = str_arg(args, "until").map_or_else(
        || {
            let dwell = num_arg(args, "idle_ms").map_or(DEFAULT_IDLE_DWELL, Duration::from_millis);
            Condition::Idle(dwell)
        },
        |needle| Condition::Contains(needle.to_owned()),
    );
    let timeout = num_arg(args, "timeout_secs").map(Duration::from_secs);

    let terminal_id = resolve_selector(&socket, &selector, false).await?;
    let result = phux_client::wait::poll_until(
        &socket,
        terminal_id,
        &condition,
        timeout,
        DEFAULT_POLL_INTERVAL,
    )
    .await?;
    let outcome = match result.outcome {
        WaitOutcome::Met => "met",
        WaitOutcome::TimedOut => "timed_out",
    };
    Ok(json!({ "outcome": outcome, "polls": result.polls }))
}

/// `phux_watch` — a bounded batch of watch items (events and `agent_state`
/// changes, in push order): stops at `max_events`, `timeout_secs`, or server
/// close. Without `target` it follows the whole local fleet.
async fn phux_watch(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_or_default(args);
    let terminal = match str_arg(args, "target") {
        None => None,
        Some(raw) => {
            let selector = parse_selector(raw)?;
            let view = state::get_state(&socket).await?;
            Some(resolve_one(&socket, &selector, &view).await?)
        }
    };
    let max_items = num_arg(args, "max_events").and_then(|n| usize::try_from(n).ok());
    let timeout = num_arg(args, "timeout_secs").map(Duration::from_secs);

    let items = collect_watch_items(&socket, terminal, max_items, timeout).await?;
    let rendered: Vec<Value> = items.iter().map(watch_item_json).collect();
    // `2`: `events[].event` gained the `agent_state` value.
    Ok(json!({ "schema_version": 2, "events": rendered, "count": rendered.len() }))
}

/// Bounded collection of both watch item kinds; `timeout` elapsing returns
/// the collected prefix, not an error.
async fn collect_watch_items(
    socket: &std::path::Path,
    terminal: Option<ResourceId>,
    max_items: Option<usize>,
    timeout: Option<Duration>,
) -> Result<Vec<WatchItem>, AttachError> {
    let mut collected: Vec<WatchItem> = Vec::new();
    {
        let sink = |item: WatchItem| {
            collected.push(item);
            max_items.is_none_or(|max| collected.len() < max)
        };
        let fut = phux_client::watch::watch_events(socket, terminal, sink);
        match timeout {
            // Timeout is a clean stop: drop the future, keep the prefix.
            Some(deadline) => {
                if let Ok(result) = tokio::time::timeout(deadline, fut).await {
                    result?;
                }
            }
            None => fut.await?,
        }
    }
    Ok(collected)
}

/// Project one [`WatchItem`] to its JSON line.
fn watch_item_json(item: &WatchItem) -> Value {
    match item {
        WatchItem::Event(event) => phux_client::watch::event_json(event),
        WatchItem::AgentState(update) => agent_state_json(update),
    }
}

/// Project one `AgentStateUpdate` to the CLI's `agent_state` line shape:
/// identity survives the tombstone, `state` is present-and-null on a
/// deletion, and `attention` is the effective level.
fn agent_state_json(update: &phux_client::watch::AgentStateUpdate) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("event".to_owned(), Value::from("agent_state"));
    if let Some(terminal) = &update.terminal {
        obj.insert(
            "terminal".to_owned(),
            Value::from(selector::format_terminal_id(terminal)),
        );
    }
    if let Some(record) = update.record.as_ref().or(update.previous.as_ref()) {
        obj.insert("name".to_owned(), Value::from(record.name.clone()));
        if let Some(kind) = &record.kind {
            obj.insert("kind".to_owned(), Value::from(kind.clone()));
        }
    }
    match &update.record {
        Some(record) => {
            obj.insert("state".to_owned(), Value::from(record.state.as_str()));
            obj.insert(
                "attention".to_owned(),
                Value::from(record.effective_attention().as_str()),
            );
            if let Some(session) = &record.session {
                obj.insert("session".to_owned(), Value::from(session.clone()));
            }
        }
        None => {
            obj.insert("state".to_owned(), Value::Null);
        }
    }
    if let Some(previous) = &update.previous {
        obj.insert("from".to_owned(), Value::from(previous.state.as_str()));
    }
    Value::Object(obj)
}
