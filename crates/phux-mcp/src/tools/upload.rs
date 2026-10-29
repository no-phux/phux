/// `phux_upload` — a managed in-process transfer, never a CLI subprocess.
async fn phux_upload(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "path", "extension", "timeout_secs", "socket"],
        &["target", "path"],
    )?;
    let selector = required_target(args)?;
    let path = upload_path(args)?;
    let extension = upload_extension(args)?;
    let secs = upload_timeout_secs(args)?;
    let socket = socket_arg(args)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let pane = tokio::time::timeout_at(deadline, async {
        resolve_selector(&socket, &selector, false).await
    })
    .await
    .map_err(|_| {
        ToolError::new("upload deadline elapsed during target resolution; no transfer started")
    })??;
    let receipt = phux_client::upload::upload_file(
        phux_client_runtime::runtime::Target::uds(socket),
        pane,
        path.into(),
        extension,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
    )
    .await
    .map_err(|err| ToolError::new(err.to_string()))?;
    upload_document(&receipt)
}

fn upload_path(args: &Value) -> Result<String, ToolError> {
    let path = crate::cli_adapter::bounded_string(args, "path", true)?
        .ok_or_else(|| ToolError::new("missing `path`"))?;
    if path.contains('\0') {
        return Err(ToolError::new("`path` must not contain NUL"));
    }
    Ok(path)
}

fn upload_extension(args: &Value) -> Result<Option<String>, ToolError> {
    let extension = crate::cli_adapter::bounded_string(args, "extension", false)?;
    if extension.as_deref().is_some_and(|ext| {
        ext.is_empty() || ext.len() > 16 || !ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return Err(ToolError::new(
            "`extension` must be 1–16 ASCII alphanumeric characters",
        ));
    }
    Ok(extension)
}

fn upload_timeout_secs(args: &Value) -> Result<u64, ToolError> {
    args.get("timeout_secs").map_or(Ok(60), |value| {
        value
            .as_u64()
            .filter(|secs| (1..=3600).contains(secs))
            .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))
    })
}

fn upload_document(receipt: &phux_client::upload::UploadReceipt) -> Result<Value, ToolError> {
    let completed = receipt.outcome == phux_client::upload::UploadOutcome::Completed;
    let document = json!({ "schema_version": 1, "receipt": receipt });
    if completed {
        Ok(document)
    } else {
        Err(ToolError::new(
            json!({ "schema_version": 1, "receipt": receipt,
            "warning": "do not blindly retry: some, all, or none of the bytes may have landed" })
            .to_string(),
        ))
    }
}
