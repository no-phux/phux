use super::{last_assistant, read_last_assistant, render};
use phux_client::agent_transcript::{
    MAX_TOOL_OUTPUT_BYTES, MAX_TOOL_SUMMARY_CHARS, MAX_TRANSCRIPT_DATA_BYTES,
};
use serde_json::{Value, json};
use std::path::Path;

fn no_transcript(_: &Path) -> Option<(String, String)> {
    None
}

fn render_json(payload: &Value) -> Option<Value> {
    render(payload.to_string().as_bytes(), 1_700, false, no_transcript)
}

/// `PHUX_AGENT_TRANSCRIPT=full`: tool entries carry their output.
fn render_full(payload: &Value) -> Option<Value> {
    render(payload.to_string().as_bytes(), 1_700, true, no_transcript)
}

#[test]
fn user_prompt_submit_becomes_a_final_user_entry() {
    let data = render_json(&json!({
        "session_id": "s1",
        "hook_event_name": "UserPromptSubmit",
        "prompt": "fix the \u{1b}[31mbuild\u{1b}[0m please"
    }))
    .expect("an entry");
    assert_eq!(
        data,
        json!({
            "provider": "claude",
            "schema": "phux.transcript/v1",
            "entry": {
                "id": "user-1700",
                "role": "user",
                "text": "fix the build please",
                "truncated": false,
                "final": true
            }
        })
    );
}

/// The default carries what the pane shows: the tool, a one-line summary,
/// and its status. Output and file contents need `full`.
#[test]
fn by_default_a_tool_entry_carries_no_output() {
    let read = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Read",
        "tool_use_id": "t1",
        "tool_input": { "file_path": "/repo/.env" },
        "tool_response": { "type": "text", "file": { "filePath": "/repo/.env", "content": "API_KEY=SECRET" } }
    });
    let data = render_json(&read).expect("an entry");
    assert_eq!(
        data["entry"]["tool"],
        json!({
            "name": "Read",
            "call_id": "t1",
            "summary": "/repo/.env",
            "status": "ok",
            "output": ""
        })
    );
    assert!(!data.to_string().contains("SECRET"));
    let full = render_full(&read).expect("an entry");
    assert_eq!(full["entry"]["tool"]["output"], "API_KEY=SECRET");

    let failed = render_json(&json!({
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
        "tool_use_id": "t2",
        "tool_input": { "command": "false" },
        "error": "SECRET exit 1"
    }))
    .expect("an entry");
    assert_eq!(failed["entry"]["tool"]["status"], "error");
    assert_eq!(failed["entry"]["tool"]["output"], "");
}

/// Summaries name what a call acts on and never carry a body.
#[test]
fn summaries_leave_out_file_contents_and_edit_bodies() {
    let summary = |input: Value| super::summarize_input(&input);
    assert_eq!(
        summary(json!({ "file_path": "a.rs", "content": "SECRET" })),
        "a.rs"
    );
    assert_eq!(
        summary(json!({ "notebook_path": "n.ipynb", "new_source": "SECRET" })),
        "n.ipynb"
    );
    assert_eq!(
        summary(json!({
            "server": "x",
            "content": "SECRET",
            "body": "SECRET",
            "nested": { "a": 1 },
            "long": "y".repeat(200)
        })),
        format!("long={} nested=\u{2026} server=x", "y".repeat(80))
    );
    assert_eq!(summary(json!(["SECRET"])), "");
    assert_eq!(summary(Value::Null), "");
}

#[test]
fn post_tool_use_becomes_a_tool_entry_keyed_by_the_call() {
    let data = render_full(&json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "toolu_01",
        "tool_input": { "command": "cargo test\n  -p phux", "description": "Run tests" },
        "tool_response": { "stdout": "ok\n", "stderr": "warning: x", "interrupted": false }
    }))
    .expect("an entry");
    assert_eq!(
        data["entry"],
        json!({
            "id": "toolu_01",
            "role": "tool",
            "text": "",
            "truncated": false,
            "final": true,
            "tool": {
                "name": "Bash",
                "call_id": "toolu_01",
                "summary": "cargo test -p phux",
                "status": "ok",
                "output": "ok\n\nwarning: x"
            }
        })
    );
}

#[test]
fn tool_errors_and_shapes_are_read() {
    let failed = render_full(&json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "mcp__phux__phux_ls",
        "tool_use_id": "t2",
        "tool_input": { "all": true },
        "tool_response": [{ "type": "text", "text": "boom" }, { "is_error": true }]
    }))
    .expect("an entry");
    let tool = &failed["entry"]["tool"];
    assert_eq!(tool["summary"], "all=true");
    assert_eq!(tool["output"], "boom\n{\"is_error\":true}");
    assert_eq!(
        tool["status"], "ok",
        "only an object-level flag is an error"
    );

    let flagged = render_full(&json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_use_id": "t3",
        "tool_input": { "file_path": "/repo/src/lib.rs", "old_string": "a" },
        "tool_response": { "is_error": true, "content": "String not found" }
    }))
    .expect("an entry");
    assert_eq!(flagged["entry"]["tool"]["status"], "error");
    assert_eq!(flagged["entry"]["tool"]["summary"], "/repo/src/lib.rs");
    assert_eq!(flagged["entry"]["tool"]["output"], "String not found");

    let failure = render_full(&json!({
        "hook_event_name": "PostToolUseFailure",
        "tool_name": "Bash",
        "tool_use_id": "t4",
        "tool_input": { "command": "false" },
        "error": "exit 1"
    }))
    .expect("an entry");
    assert_eq!(failure["entry"]["tool"]["status"], "error");
    assert_eq!(failure["entry"]["tool"]["output"], "exit 1");
}

#[test]
fn tool_fields_stay_within_their_caps() {
    let data = render_full(&json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_use_id": "t5",
        "tool_input": { "command": "x".repeat(5_000) },
        "tool_response": { "stdout": format!("{}END", "y".repeat(100_000)) }
    }))
    .expect("an entry");
    let tool = &data["entry"]["tool"];
    assert_eq!(
        tool["summary"].as_str().unwrap().chars().count(),
        MAX_TOOL_SUMMARY_CHARS
    );
    let output = tool["output"].as_str().unwrap();
    assert_eq!(output.len(), MAX_TOOL_OUTPUT_BYTES);
    assert!(output.ends_with("END"));
}

#[test]
fn stop_prefers_the_supplied_message_then_the_transcript() {
    let supplied = render_json(&json!({
        "hook_event_name": "Stop",
        "transcript_path": "/nonexistent",
        "last_assistant_message": "All green."
    }))
    .expect("an entry");
    assert_eq!(supplied["entry"]["id"], "assistant-1700");
    assert_eq!(supplied["entry"]["text"], "All green.");

    let payload = json!({ "hook_event_name": "Stop", "transcript_path": "/t.jsonl" });
    let read = render(payload.to_string().as_bytes(), 1, false, |path| {
        assert_eq!(path, Path::new("/t.jsonl"));
        Some(("msg_9".to_owned(), "Done.".to_owned()))
    })
    .expect("an entry");
    assert_eq!(read["entry"]["id"], "assistant-msg_9");
    assert_eq!(read["entry"]["role"], "assistant");
    assert_eq!(read["entry"]["final"], true);

    assert_eq!(
        render_json(&payload),
        None,
        "an unreadable transcript emits nothing"
    );
}

#[test]
fn a_long_assistant_message_is_cut_to_fit_the_record() {
    let data = render_json(&json!({
        "hook_event_name": "Stop",
        "last_assistant_message": "\"quoted\" ".repeat(10_000)
    }))
    .expect("an entry");
    assert!(data.to_string().len() <= MAX_TRANSCRIPT_DATA_BYTES);
    assert_eq!(data["entry"]["truncated"], true);
}

#[test]
fn other_events_and_bad_payloads_print_nothing() {
    for event in ["SessionStart", "PreToolUse", "Notification", "SessionEnd"] {
        assert_eq!(
            render_json(&json!({ "hook_event_name": event, "prompt": "x" })),
            None
        );
    }
    assert_eq!(render(b"", 0, false, no_transcript), None);
    assert_eq!(render(b"not json", 0, false, no_transcript), None);
    assert_eq!(
        render_json(&json!({ "hook_event_name": "UserPromptSubmit", "prompt": "  " })),
        None
    );
    assert_eq!(
        render_json(&json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" })),
        None,
        "a tool entry needs its call id"
    );
}

#[test]
fn the_last_assistant_message_joins_its_text_blocks() {
    let lines = [
        json!({"type":"user","message":{"role":"user","content":"go"}}),
        json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"Earlier."}]}}),
        json!({"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}),
        json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"thinking","thinking":"hmm"}]}}),
        json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"text","text":"First."}]}}),
        json!({"type":"assistant","message":{"id":"msg_2","content":[{"type":"text","text":"Second."}]}}),
        json!({"type":"system","subtype":"stop_hook_summary"}),
    ];
    let jsonl = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        last_assistant(&format!("{jsonl}\n{{truncated")),
        Some(("msg_2".to_owned(), "First.\nSecond.".to_owned()))
    );
    assert_eq!(last_assistant("{\"type\":\"user\"}\n"), None);

    let dir = tempfile::tempdir().expect("scratch dir");
    let path = dir.path().join("t.jsonl");
    std::fs::write(&path, &jsonl).unwrap();
    assert_eq!(
        read_last_assistant(&path).map(|(id, _)| id),
        Some("msg_2".to_owned())
    );
    assert_eq!(read_last_assistant(&dir.path().join("missing")), None);
}
