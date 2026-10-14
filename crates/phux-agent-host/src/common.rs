use std::process::{Child, Command, Stdio};
use phux_client_core::session::agent_transcript::{TranscriptEntry, TranscriptRole, TranscriptTool, ToolStatus};
use phux_protocol::native_agent::*;
use serde_json::Value;
use crate::{HostError, HostSpec, Sink};

pub(super) fn result(id: u64, disposition: NativeDisposition, detail: &str) -> NativeCommandResult {
    NativeCommandResult { operation_id: id, disposition, detail: Some(head(detail, 1024)) }
}
pub(super) fn head(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_owned()
}
pub(super) fn configure(command: &mut Command, spec: &HostSpec) {
    command.current_dir(&spec.cwd).envs(&spec.env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    for (name, _) in std::env::vars().chain(spec.env.iter().map(|(key, value)| (key.clone(), value.clone()))) {
        if name.starts_with("PHUX_NATIVE_") { command.env_remove(name); }
    }
    command.env("PHUX_AGENT_REPORTING", "0").env("PHUX_TERMINAL_ID", spec.parent.to_string());
    // PHUX_SOCKET stays the launcher's delegated tool socket, not an owner socket.
    command.env("PHUX_SOCKET", &spec.socket);
}
/// `from_io` does not own or reap a child. Kill the individual worker, NEVER
/// its process group (it is the guardian's own group), before SDK reader drop.
pub(super) struct ChildGuard(pub Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) { let _ = self.0.kill(); }
        let _ = self.0.wait();
    }
}

/// Monitor close even while an official blocking SDK is initializing/calling.
pub(super) struct WatchedChild(pub std::sync::Arc<parking_lot::Mutex<Child>>, std::sync::Arc<std::sync::atomic::AtomicBool>);
impl WatchedChild {
    pub(super) fn spawn(mut command: Command, shutdown: tokio::sync::watch::Receiver<bool>) -> Result<Self, HostError> {
        let child = std::sync::Arc::new(parking_lot::Mutex::new(command.spawn()?));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let monitor_child = child.clone(); let monitor_done = done.clone();
        std::thread::spawn(move || {
            while !monitor_done.load(std::sync::atomic::Ordering::Acquire) {
                if *shutdown.borrow() || shutdown.has_changed().is_err() {
                    let mut child = monitor_child.lock();
                    if !matches!(child.try_wait(), Ok(Some(_))) { let _ = child.kill(); }
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
        Ok(Self(child, done))
    }
}
impl Drop for WatchedChild {
    fn drop(&mut self) {
        self.1.store(true, std::sync::atomic::Ordering::Release);
        let mut child = self.0.lock();
        if !matches!(child.try_wait(), Ok(Some(_))) { let _ = child.kill(); }
        let _ = child.wait();
    }
}
/// An inherited stdout in an upstream background process must not deadlock
/// AgentSession-only close. Direct child reaping remains unconditional.
pub(super) fn join_reader(thread: std::thread::JoinHandle<Result<(), HostError>>, name: &str) -> Result<Result<(), HostError>, HostError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !thread.is_finished() && std::time::Instant::now() < deadline { std::thread::sleep(std::time::Duration::from_millis(10)); }
    if !thread.is_finished() { return Err(HostError::Sdk(format!("{name} stdout did not close after worker termination"))); }
    thread.join().map_err(|_| HostError::Sdk(format!("{name} panicked")))
}
pub(super) fn text(sink: &Sink, provider: &str, id: String, role: TranscriptRole, text: String, final_row: bool) -> Result<(), HostError> {
    transcript(sink, provider, TranscriptEntry { id, role, text, truncated: false, is_final: final_row, tool: None })
}
pub(super) fn tool(sink: &Sink, provider: &str, id: String, name: String, summary: String, output: String, status: ToolStatus) -> Result<(), HostError> {
    transcript(sink, provider, TranscriptEntry { id: id.clone(), role: TranscriptRole::Tool, text: String::new(), truncated: false, is_final: status != ToolStatus::Running,
        tool: Some(TranscriptTool { name, call_id: id, summary, output, status }) })
}
fn transcript(sink: &Sink, provider: &str, entry: TranscriptEntry) -> Result<(), HostError> {
    // Use the existing transcript sanitizer and size ceiling, with room for the
    // native wrapper as well as the server-stamped record envelope.
    let mut entry = entry;
    if entry.text.len() > 12 * 1024 { entry.text = head(&entry.text, 12 * 1024); entry.truncated = true; }
    let data = entry.into_data(provider);
    let mut entry = data["entry"].clone();
    loop {
        let update = NativeUpdate::Transcript { entry: entry.clone() };
        let encoded = serde_json::to_vec(&update).map_err(|error| HostError::Sdk(error.to_string()))?;
        if encoded.len() <= 14 * 1024 { return sink.update(update); }
        let over = encoded.len() - 14 * 1024;
        if let Some(text) = entry["text"].as_str().filter(|text| !text.is_empty()) {
            let text = head(text, text.len().saturating_sub(over));
            entry["text"] = Value::String(text); entry["truncated"] = Value::Bool(true);
        } else if let Some(output) = entry["tool"]["output"].as_str().filter(|output| !output.is_empty()) {
            let mut start = output.len().min(over);
            while !output.is_char_boundary(start) { start += 1; }
            entry["tool"]["output"] = Value::String(output[start..].into());
            entry["truncated"] = Value::Bool(true);
        } else { return Err(HostError::Invalid("transcript metadata exceeds native record bounds".into())); }
    }
}
pub(super) fn validate_answer(question: &NativeQuestion, response: &Value) -> Result<(), String> {
    if response.get("cancelled").and_then(Value::as_bool) == Some(true) { return Ok(()); }
    match question.kind.as_str() {
        "confirm" if response.get("confirmed").and_then(Value::as_bool).is_some() => Ok(()),
        "input" | "editor" if response.get("text").and_then(Value::as_str).is_some() => Ok(()),
        "select" | "permission" => {
            let id = response.get("option_id").and_then(Value::as_str).ok_or("option_id required")?;
            if question.options.iter().any(|option| option.id == id) { Ok(()) } else { Err("unknown provider option".into()) }
        }
        "ask" => {
            let answers = response.get("answers").and_then(Value::as_array).ok_or("answers required")?;
            if answers.len() != question.questions.len() { return Err("answer every question exactly once".into()); }
            let mut seen = std::collections::BTreeSet::new();
            for answer in answers {
                let id = answer.get("id").and_then(Value::as_str).ok_or("answer id required")?;
                if !seen.insert(id) { return Err("duplicate question answer".into()); }
                let item = question.questions.iter().find(|item| item.id == id).ok_or("unknown question id")?;
                let selected = answer.get("selected_options").and_then(Value::as_array).ok_or("selected_options required")?;
                if !item.multi && selected.len() > 1 { return Err("question is single-select".into()); }
                let mut options = std::collections::BTreeSet::new();
                for option in selected {
                    let id = option.as_str().ok_or("option id must be text")?;
                    if !options.insert(id) || !item.options.iter().any(|option| option.id == id) { return Err("invalid or duplicate option id".into()); }
                }
                if answer.get("custom_input").is_some_and(|input| !input.is_string()) { return Err("custom_input must be text".into()); }
                if question.fields["allow_custom_input"].as_bool() == Some(false) && answer.get("custom_input").and_then(Value::as_str).is_some_and(|input| !input.is_empty()) {
                    return Err("provider does not support custom answers".into());
                }
            }
            Ok(())
        }
        "form" if response.get("values").is_some_and(Value::is_object) => Ok(()),
        _ => Err("invalid native question response".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_never_defaults_to_approval() {
        let question = NativeQuestion { id: "p".into(), kind: "permission".into(), title: String::new(), message: String::new(), options: vec![NativeOption { id: "actual-id".into(), label: "Allow once".into(), description: None }], questions: Vec::new(), fields: Value::Null };
        assert!(validate_answer(&question, &serde_json::json!({})).is_err());
        assert!(validate_answer(&question, &serde_json::json!({"option_id":"allow"})).is_err());
        assert!(validate_answer(&question, &serde_json::json!({"option_id":"actual-id"})).is_ok());
    }
    #[test]
    fn truncation_respects_utf8() { assert_eq!(head("éé", 3), "é"); }
    #[cfg(unix)]
    #[test]
    fn session_close_kills_and_reaps_even_during_blocking_sdk_startup() {
        let (close, closed) = tokio::sync::watch::channel(false);
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let guard = WatchedChild::spawn(command, closed).unwrap();
        let child = guard.0.clone();
        assert!(child.lock().try_wait().unwrap().is_none());
        close.send(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while child.lock().try_wait().unwrap().is_none() && std::time::Instant::now() < deadline { std::thread::sleep(std::time::Duration::from_millis(10)); }
        assert!(child.lock().try_wait().unwrap().is_some());
        drop(guard);
        assert!(child.lock().try_wait().unwrap().is_some());
    }
}
