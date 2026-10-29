/// `phux_send_keys` — send input to the pane named by the selector.
async fn phux_send_keys(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_or_default(args);
    let selector = required_target(args)?;
    let keys = string_array(args, "keys")?;
    if keys.is_empty() {
        return Err(ToolError::new(
            "`keys` must be a non-empty array of strings",
        ));
    }
    let pane = resolve_selector(&socket, &selector, true).await?;
    phux_client::send_keys::send_to(&socket, pane.clone(), &keys).await?;
    Ok(json!({ "sent": true, "pane": pane_value(&pane) }))
}

/// `phux_paste` — one paste event into the pane named by the selector;
/// trusted by default, as `phux paste`.
async fn phux_paste(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "text", "untrusted", "socket"],
        &["target", "text"],
    )?;
    let socket = socket_or_default(args);
    let selector = required_target(args)?;
    let text = required_str(args, "text")?.to_owned();
    let untrusted = bool_arg(args, "untrusted").unwrap_or(false);
    let trust = if untrusted {
        PasteTrust::Untrusted
    } else {
        PasteTrust::Trusted
    };
    let pane = resolve_selector(&socket, &selector, true).await?;
    phux_client::send_keys::paste_to(&socket, pane.clone(), text.into_bytes(), trust).await?;
    Ok(json!({ "sent": true, "pane": pane_value(&pane), "untrusted": untrusted }))
}

/// `phux_run` — run a command in the pane named by the selector.
async fn phux_run(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "command", "timeout_secs", "force", "socket"],
        &["target", "command"],
    )?;
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let command = crate::cli_adapter::bounded_string(args, "command", true)?.unwrap_or_default();
    let timeout_secs = args
        .get("timeout_secs")
        .map_or(Ok(RUN_DEFAULT_TIMEOUT_SECS), |value| {
            value
                .as_u64()
                .filter(|value| (1..=3600).contains(value))
                .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))
        })?;
    let mut argv = vec![
        "run".to_owned(),
        "--json".to_owned(),
        "--timeout".to_owned(),
        timeout_secs.to_string(),
    ];
    if bool_arg(args, "force").unwrap_or(false) {
        argv.push("--force".to_owned());
    }
    crate::cli_adapter::push_socket(&mut argv, args)?;
    argv.extend([target, command]);
    // A failing command is a successful call reporting its `exit_code`.
    crate::cli_adapter::CliAdapter::for_residue("phux_run")?
        .run_json_mirrored_exit(argv, Duration::from_secs(timeout_secs.saturating_add(5)))
        .await
}

/// Default `phux_run` timeout, matching `phux run`'s.
const RUN_DEFAULT_TIMEOUT_SECS: u64 = 600;
