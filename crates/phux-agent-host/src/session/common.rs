use super::{HostError, HostSpec, Sink};
use phux_client_core::session::agent_transcript::{
    ToolStatus, TranscriptEntry, TranscriptRole, TranscriptTool,
};
use phux_protocol::native_agent::{
    NativeCommandResult, NativeDisposition, NativeQuestion, NativeUpdate,
};
use serde_json::Value;
use std::process::{Command, Stdio};

pub(super) fn result(id: u64, disposition: NativeDisposition, detail: &str) -> NativeCommandResult {
    NativeCommandResult {
        operation_id: id,
        disposition,
        detail: Some(head(detail, 1024)),
    }
}
pub(super) fn head(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}
pub(super) fn configure(command: &mut Command, spec: &HostSpec) {
    worker_env(command, &spec.env);
    command
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    command
        .env("PHUX_AGENT_REPORTING", "0")
        .env("PHUX_TERMINAL_ID", spec.parent.to_string());
    // PHUX_SOCKET stays the launcher's delegated tool socket, not an owner socket.
    command.env("PHUX_SOCKET", &spec.socket);
}
fn worker_env(command: &mut Command, env: &std::collections::BTreeMap<String, String>) {
    command.env_clear().envs(env.iter().filter(|(name, _)| {
        // Carry only explicit safe tool routing/UX context. Future PHUX_* state
        // and inherited production transports/credentials are denied by default.
        !name.starts_with("PHUX_")
            || matches!(
                name.as_str(),
                "PHUX_BIN"
                    | "PHUX_SOCKET"
                    | "PHUX_TERMINAL_ID"
                    | "PHUX_AGENT_REPORTING"
                    | "PHUX_AGENT_TRANSCRIPT"
                    | "PHUX_AUTO_SPAWN_EXIT_AFTER_IDLE"
            )
    }));
}

pub(super) fn text(
    sink: &Sink,
    provider: &str,
    id: String,
    role: TranscriptRole,
    text: String,
    final_row: bool,
) -> Result<(), HostError> {
    transcript(
        sink,
        provider,
        TranscriptEntry {
            id,
            role,
            text,
            truncated: false,
            is_final: final_row,
            tool: None,
        },
    )
}
pub(super) fn tool(
    sink: &Sink,
    provider: &str,
    id: String,
    name: String,
    summary: String,
    output: String,
    status: ToolStatus,
) -> Result<(), HostError> {
    transcript(
        sink,
        provider,
        TranscriptEntry {
            id: id.clone(),
            role: TranscriptRole::Tool,
            text: String::new(),
            truncated: false,
            is_final: status != ToolStatus::Running,
            tool: Some(TranscriptTool {
                name,
                call_id: id,
                summary,
                output,
                status,
            }),
        },
    )
}
fn transcript(sink: &Sink, provider: &str, mut entry: TranscriptEntry) -> Result<(), HostError> {
    if sink.0.transcript_mode == TranscriptMode::Off {
        return Ok(());
    }
    if sink.0.transcript_mode != TranscriptMode::Full
        && let Some(tool) = &mut entry.tool
    {
        tool.output.clear();
    }
    let mut update = NativeUpdate::Transcript {
        entry: entry.into_data(provider),
    };
    loop {
        let bytes = super::json_size(&update)?;
        if bytes <= super::UPDATE_BYTES {
            return sink.update(update);
        }
        let NativeUpdate::Transcript { entry: data } = &mut update else {
            unreachable!()
        };
        let entry = &mut data["entry"];
        let over = bytes - super::UPDATE_BYTES;
        let field = if entry["text"].as_str().is_some_and(|text| !text.is_empty()) {
            &mut entry["text"]
        } else if entry["tool"]["output"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
        {
            &mut entry["tool"]["output"]
        } else {
            return Err(HostError::Invalid(
                "transcript metadata exceeds native record bounds".into(),
            ));
        };
        let text = field
            .as_str()
            .ok_or_else(|| HostError::Invalid("transcript text field is not a string".into()))?;
        *field = Value::String(head(text, text.len().saturating_sub(over)));
        entry["truncated"] = Value::Bool(true);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TranscriptMode {
    Off,
    Content,
    Full,
}
impl TranscriptMode {
    pub(super) fn from_env(value: Option<&str>) -> Self {
        match value {
            Some("0") => Self::Off,
            Some("full") => Self::Full,
            _ => Self::Content,
        }
    }
}

/// Match the runtime's content-excluding argument summary; never dump raw args.
pub(super) fn summarize_args(args: &Value) -> String {
    fn line(text: &str, limit: usize) -> String {
        let mut words = text.split_whitespace();
        words
            .next()
            .unwrap_or_default()
            .chars()
            .chain(words.flat_map(|word| std::iter::once(' ').chain(word.chars())))
            .take(limit)
            .collect()
    }
    let Some(args) = args.as_object() else {
        return args
            .as_str()
            .map(|text| line(text, 512))
            .unwrap_or_default();
    };
    for key in [
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ] {
        if let Some(text) = args
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        {
            return line(text, 512);
        }
    }
    // Match the stable summary even when the SDK enables preserve_order.
    let mut fields: Vec<_> = args.iter().collect();
    fields.sort_unstable_by_key(|(key, _)| *key);
    let mut pairs = String::new();
    for (key, value) in fields {
        if matches!(
            key.as_str(),
            "content"
                | "contents"
                | "new_string"
                | "old_string"
                | "newText"
                | "oldText"
                | "new_source"
                | "edits"
                | "text"
                | "body"
                | "data"
                | "patch"
                | "diff"
        ) {
            continue;
        }
        let value = match value {
            Value::String(text) => line(text, 80),
            Value::Array(_) | Value::Object(_) => "…".into(),
            _ => value.to_string(),
        };
        if !pairs.is_empty() {
            pairs.push(' ');
        }
        pairs.push_str(key);
        pairs.push('=');
        pairs.push_str(&value);
        if pairs.len() >= 2048 {
            break;
        }
    }
    line(&pairs, 512)
}
pub(super) fn validate_answer(question: &NativeQuestion, response: &Value) -> Result<(), String> {
    phux_protocol::native_agent::validate_answer(question, response).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::native_agent::NativeOption;
    #[test]
    fn worker_process_receives_only_sanitized_explicit_environment() {
        let mut command = Command::new("/usr/bin/env");
        let env = std::collections::BTreeMap::from([
            ("PHUX_NATIVE_BINDING".into(), "secret".into()),
            ("PHUX_PROFILE".into(), "default".into()),
            ("PHUX_WS_ADDR".into(), "production".into()),
            ("PHUX_FUTURE_PRIVATE_KEY".into(), "secret".into()),
            ("PHUX_WS_TOKENS".into(), "production".into()),
            ("PHUX_WS_TLS_CERT".into(), "certificate".into()),
            ("PHUX_WS_TLS_KEY".into(), "key".into()),
            ("PHUX_SOCKET".into(), "delegated".into()),
        ]);
        worker_env(&mut command, &env);
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            "PHUX_SOCKET=delegated"
        );
    }
    #[test]
    fn permission_never_defaults_to_approval() {
        let question = NativeQuestion {
            id: "p".into(),
            kind: "permission".into(),
            title: String::new(),
            message: String::new(),
            options: vec![NativeOption {
                id: "actual-id".into(),
                label: "Allow once".into(),
                description: None,
            }],
            questions: Vec::new(),
            fields: Value::Null,
        };
        assert!(validate_answer(&question, &serde_json::json!({})).is_err());
        assert!(validate_answer(&question, &serde_json::json!({"option_id":"allow"})).is_err());
        assert!(validate_answer(&question, &serde_json::json!({"option_id":"actual-id"})).is_ok());
    }
    #[test]
    fn truncation_respects_utf8() {
        assert_eq!(head("éé", 3), "é");
    }
}
