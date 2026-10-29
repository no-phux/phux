/// `phux_new` — `phux new --json` for a named session, without attaching.
async fn phux_new(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "name",
            "lifetime",
            "command",
            "cwd",
            "idempotency_key",
            "socket",
        ],
        &["name"],
    )?;
    let name = crate::cli_adapter::bounded_string(args, "name", true)?.unwrap_or_default();
    let lifetime = crate::cli_adapter::enum_string(
        args,
        "lifetime",
        &["persistent", "ephemeral"],
        Some("persistent"),
    )?;
    let mut argv = vec!["new".to_owned(), "-s".to_owned(), name, "--json".to_owned()];
    if lifetime == "ephemeral" {
        argv.push("--ephemeral".to_owned());
    }
    if let Some(cwd) = crate::cli_adapter::bounded_string(args, "cwd", false)? {
        argv.extend(["-c".to_owned(), cwd]);
    }
    if let Some(key) = crate::cli_adapter::bounded_string(args, "idempotency_key", false)? {
        argv.extend(["--idempotency-key".to_owned(), key]);
    }
    crate::cli_adapter::push_socket(&mut argv, args)?;
    let command = crate::cli_adapter::bounded_strings(args, "command", false)?;
    if !command.is_empty() {
        argv.push("--".to_owned());
        argv.extend(command);
    }
    crate::cli_adapter::CliAdapter::for_residue("phux_new")?
        .run_json(argv, crate::cli_adapter::DEFAULT_CALL_TIMEOUT)
        .await
}

/// `phux_kill` — tear down the Terminal(s) a selector resolves to, through
/// [`phux_client::kill::selected`], the path `phux kill` calls.
async fn phux_kill(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "confirm", "idempotency_key", "socket"],
        &["target", "confirm"],
    )?;
    let target = crate::cli_adapter::bounded_string(args, "target", true)?.unwrap_or_default();
    let key = crate::cli_adapter::bounded_string(args, "idempotency_key", false)?
        .as_deref()
        .map(crate::pane_tools::idempotency_key)
        .transpose()?;
    let socket = socket_arg(args)?;
    let selector = parse_selector(&target)?;
    let mut conn = Connection::connect(&socket).await?;
    let mut notices = Vec::new();
    let result =
        phux_client::kill::selected(&mut conn, &selector, &target, key, &mut notices).await;
    drop(conn);
    warn_partial_view("kill", &notices);
    match result {
        Ok(done) => {
            let mut document = json!({ "schema_version": 1, "killed": true, "target": target });
            if let Some(members) = done.killed {
                document["members"] = json!(members);
            }
            Ok(document)
        }
        Err(err) => Err(kill_error(err)),
    }
}

/// Print the CLI's partial-fleet-view warning on stderr, the adapter's
/// out-of-band diagnostic channel.
pub(crate) fn warn_partial_view(verb: &str, notices: &[String]) {
    for notice in notices {
        let message = phux_client::state::partial_view_warning(verb, notice);
        // Adapt the shared warning header, keeping the fleet detail intact.
        let payload = message.strip_prefix("phux: warning: ").unwrap_or(&message);
        crate::diagnostic("warn", format_args!("{payload}"));
    }
}

/// `phux_detach` — `DETACH_CLIENTS` from outside the attach UI, reporting
/// how many clients the server actually disconnected.
async fn phux_detach(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["session", "confirm", "socket"], &["confirm"])?;
    let socket = socket_or_default(args);
    let session = crate::cli_adapter::bounded_string(args, "session", false)?;
    let mut conn = Connection::connect(&socket).await?;
    let outcome = phux_client::detach::detach_clients(&mut conn, 1, session.clone()).await;
    drop(conn);
    match outcome {
        Ok((DetachOutcome::Detached(detached), _)) => {
            Ok(json!({ "schema_version": 1, "detached": detached, "session": session }))
        }
        Ok((DetachOutcome::NoSuchSession, _)) => Err(ToolError::new(format!(
            "no such session: {}",
            session.as_deref().unwrap_or_default()
        ))),
        Ok((DetachOutcome::Malformed(count), _)) => Err(ToolError::new(format!(
            "phux detach returned a malformed count: {count:?}"
        ))),
        Ok((DetachOutcome::Refused(message), _)) => {
            Err(ToolError::new(format!("detach refused: {message}")))
        }
        Ok((DetachOutcome::Unexpected(other), _)) => Err(ToolError::new(
            phux_client::explain::explain_unexpected("detach", &other),
        )),
        // Detaching never stops the server: a disconnect is unconfirmed.
        Err(AttachError::Disconnected) => {
            Err(ToolError::new("connection closed before the detach reply"))
        }
        Err(err) => Err(err.into()),
    }
}
