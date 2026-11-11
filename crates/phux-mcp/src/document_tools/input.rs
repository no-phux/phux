//! The document input tools: one typed intent per tool, sent to a
//! document's producer with `SEND_DOCUMENT_INPUT`, as `phux agent page
//! navigate|click|type|key|scroll` sends it. One tool per intent keeps each
//! schema flat and lets a client approve "type" without approving "navigate".

use phux_client::attach::connection::Connection;
use phux_client::selector::format_terminal_id;
use phux_client::tree::{self, DocumentInput, DocumentKey, TreeError};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{ControlClass, MAX_DOCUMENT_INPUT_TEXT};
use serde_json::{Value, json};

use super::SCHEMA_VERSION;
use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, required_str, strict_object};

const LOOP: &str = "Loop: phux_documents to find the surface and its `control`, phux_surface_snapshot to see it, act, then snapshot again to confirm.";
const AUTHORITY: &str = "Needs a document whose `control` (from phux_documents) is page, window or display, and the owner socket or an `input@<control>` grant. Not read-only, and destructive: input can do anything the page or desktop allows.";

pub(super) fn schemas() -> Vec<Value> {
    let target = || json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": "The document resource (its surface or tree), by the `@N` id phux_documents lists." });
    let text = |what: &str| json!({ "type": "string", "minLength": 1, "maxLength": MAX_DOCUMENT_INPUT_TEXT, "description": what });
    let pixel = |what: &str| json!({ "type": "integer", "minimum": 0, "maximum": u32::MAX, "description": what });
    let delta = |what: &str| json!({ "type": "integer", "minimum": i32::MIN, "maximum": i32::MAX, "description": what });
    let keys: Vec<&str> = DocumentKey::ALL.iter().map(|key| key.name()).collect();
    let entry = |name: &str, what: &str, mut props: Value, required: &[&str]| {
        props["target"] = target();
        props["socket"] = string_schema();
        let mut all = vec!["target"];
        all.extend(required);
        schema(
            name,
            &format!("{what} {LOOP} {AUTHORITY} Returns `sent` with the document id."),
            props,
            &all,
        )
    };
    vec![
        entry(
            "phux_document_navigate",
            "Load a URL in a mirrored browser page (`control` page), as `phux agent page navigate`.",
            json!({ "url": text("The URL to load.") }),
            &["url"],
        ),
        entry(
            "phux_document_click",
            "Click a document at a point of its surface image, in that image's pixels: read `x` and `y` off the PNG phux_surface_snapshot returns. As `phux agent page click`.",
            json!({ "x": pixel("Pixels from the image's left edge."), "y": pixel("Pixels from the image's top edge.") }),
            &["x", "y"],
        ),
        entry(
            "phux_document_type",
            "Insert text at the document's focus (click a field first), as `phux agent page type`.",
            json!({ "text": text("The text to insert.") }),
            &["text"],
        ),
        entry(
            "phux_document_key",
            "Press and release one named key in the document, as `phux agent page key`.",
            json!({ "key": { "type": "string", "enum": keys } }),
            &["key"],
        ),
        entry(
            "phux_document_scroll",
            "Scroll the document by a delta in surface image pixels, as `phux agent page scroll`.",
            json!({ "dx": delta("Horizontal pixels; positive scrolls right."), "dy": delta("Vertical pixels; positive scrolls down.") }),
            &["dx", "dy"],
        ),
    ]
}

pub(super) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_document_navigate"
            | "phux_document_click"
            | "phux_document_type"
            | "phux_document_key"
            | "phux_document_scroll"
    )
}

pub(super) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    let input = intent(name, args)?;
    let (socket, id) = crate::resource_tools::target(args).await?;
    let control = control_of(&socket, &id).await?;
    let mut conn = Connection::connect(&socket).await?;
    match tree::send_input(&mut conn, &id, input).await {
        Ok(()) => Ok(json!({ "schema_version": SCHEMA_VERSION, "sent": format_terminal_id(&id) })),
        Err(err) => Err(input_error(&id, control, &err)),
    }
}

/// The intent `name`'s arguments describe, checked before any connection.
fn intent(name: &str, args: &Value) -> Result<DocumentInput, ToolError> {
    let fields: &[&str] = match name {
        "phux_document_navigate" => &["url"],
        "phux_document_click" => &["x", "y"],
        "phux_document_type" => &["text"],
        "phux_document_key" => &["key"],
        "phux_document_scroll" => &["dx", "dy"],
        other => return Err(ToolError::new(format!("unknown document tool: {other}"))),
    };
    let mut allowed = vec!["target", "socket"];
    allowed.extend(fields);
    let mut required = vec!["target"];
    required.extend(fields);
    strict_object(args, &allowed, &required)?;
    let int = |key: &str| {
        args.get(key)
            .and_then(Value::as_i64)
            .ok_or_else(|| ToolError::new(format!("`{key}` must be an integer")))
    };
    let pixel = |key: &str| {
        u32::try_from(int(key)?)
            .map_err(|_| ToolError::new(format!("`{key}` must be a pixel in 0..={}", u32::MAX)))
    };
    let delta = |key: &str| {
        i32::try_from(int(key)?)
            .map_err(|_| ToolError::new(format!("`{key}` must fit a 32-bit signed integer")))
    };
    let text = |key: &str| -> Result<String, ToolError> {
        let text = required_str(args, key)?;
        if text.is_empty() || text.len() > MAX_DOCUMENT_INPUT_TEXT {
            return Err(ToolError::new(format!(
                "`{key}` must be 1..={MAX_DOCUMENT_INPUT_TEXT} bytes"
            )));
        }
        Ok(text.to_owned())
    };
    Ok(match name {
        "phux_document_navigate" => DocumentInput::Navigate { url: text("url")? },
        "phux_document_click" => DocumentInput::Click {
            x: pixel("x")?,
            y: pixel("y")?,
        },
        "phux_document_type" => DocumentInput::Type {
            text: text("text")?,
        },
        "phux_document_key" => {
            let key = required_str(args, "key")?;
            let parsed = DocumentKey::parse(key).ok_or_else(|| {
                let keys: Vec<&str> = DocumentKey::ALL.iter().map(|key| key.name()).collect();
                ToolError::new(format!(
                    "unknown key {key:?}: name one of {}",
                    keys.join(", ")
                ))
            })?;
            DocumentInput::Key { key: parsed }
        }
        _ => DocumentInput::Scroll {
            dx: delta("dx")?,
            dy: delta("dy")?,
        },
    })
}

/// The document's control class, refusing one that has none: its producer
/// declared that nothing acts on input. A document the view does not list
/// is left for the server to judge.
async fn control_of(
    socket: &std::path::Path,
    id: &ResourceId,
) -> Result<Option<ControlClass>, ToolError> {
    let snapshot = phux_client::state::get_state(socket)
        .await?
        .into_snapshot_ignoring_degradation();
    let Some(info) = snapshot.resources.iter().find(|info| info.id == *id) else {
        return Ok(None);
    };
    info.control.map(Some).ok_or_else(|| {
        refusal(
            "no_control_class",
            &format!(
                "{} accepts no input: it has no control class",
                format_terminal_id(id)
            ),
            "send input to a document whose `control` in phux_documents is page, window or display; a producer declares it when it opens the document (`phux agent surface open --control CLASS`)",
        )
    })
}

fn input_error(id: &ResourceId, control: Option<ControlClass>, err: &TreeError) -> ToolError {
    match err {
        TreeError::NoProducer(message) => refusal(
            "no_producer",
            &format!("{} has no producer: {message}", format_terminal_id(id)),
            "the process feeding it (e.g. `phux agent page mirror`) is gone; restart it and use the new id from phux_documents",
        ),
        // `map_refusal` spells an unmapped code as `{code:?}: message`.
        TreeError::Refused(message) if message.starts_with("PermissionDenied") => {
            let class = control.map_or("CLASS", ControlClass::name);
            refusal(
                "permission_denied",
                &format!(
                    "this connection lacks an input@{class} grant for {}: {message}",
                    format_terminal_id(id)
                ),
                &format!(
                    "the owner can add one with `phux workload grant CREDENTIAL_ID input@{class}` (or `input@resource:KEY` for this document alone)"
                ),
            )
        }
        other => ToolError::new(other.to_string()),
    }
}

fn refusal(code: &str, message: &str, remedy: &str) -> ToolError {
    let document = json!({
        "schema_version": SCHEMA_VERSION,
        "error": { "code": code, "message": message },
        "remedy": remedy,
    });
    ToolError::with_document(format!("{message}; {remedy}"), document)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use std::time::Duration;

    use phux_client::tree::Kind;
    use phux_server_testkit::{
        SOCKET_CONNECT_DEADLINE, join_after_shutdown, run_local, spawn_server_with, wait_for_socket,
    };

    use super::*;

    async fn next_intent(producer: &mut Connection) -> DocumentInput {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(intent) = tree::intent(&producer.recv().await.unwrap()) {
                    return intent.input;
                }
            }
        })
        .await
        .expect("an intent reaches the producer")
    }

    #[test]
    fn each_tool_sends_its_intent_and_a_classless_document_refuses() {
        run_local(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("input.sock");
            let (shutdown, server) = spawn_server_with(socket.clone(), Some("main"), |_| {});
            drop(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
            let view = phux_client::state::get_state(&socket).await.unwrap();
            let pane = phux_client::resource::terminals(&view.into_snapshot_ignoring_degradation())
                .next()
                .unwrap()
                .id
                .clone();
            let mut producer = Connection::connect(&socket).await.unwrap();
            let controlled = tree::open_kind(
                &mut producer,
                Kind::Surface,
                &pane,
                "page",
                Some(ControlClass::Page),
            )
            .await
            .unwrap()
            .resource;
            let bare = tree::open_kind(&mut producer, Kind::Surface, &pane, "bare", None)
                .await
                .unwrap()
                .resource;
            let page_id = format_terminal_id(&controlled);

            for (tool, args, want) in [
                (
                    "phux_document_navigate",
                    json!({ "url": "https://phux.sh/" }),
                    DocumentInput::Navigate {
                        url: "https://phux.sh/".into(),
                    },
                ),
                (
                    "phux_document_click",
                    json!({ "x": 12, "y": 34 }),
                    DocumentInput::Click { x: 12, y: 34 },
                ),
                (
                    "phux_document_type",
                    json!({ "text": "hello" }),
                    DocumentInput::Type {
                        text: "hello".into(),
                    },
                ),
                (
                    "phux_document_key",
                    json!({ "key": "Enter" }),
                    DocumentInput::Key {
                        key: DocumentKey::Enter,
                    },
                ),
                (
                    "phux_document_scroll",
                    json!({ "dx": -5, "dy": 120 }),
                    DocumentInput::Scroll { dx: -5, dy: 120 },
                ),
            ] {
                let mut args = args;
                args["target"] = json!(page_id);
                args["socket"] = json!(socket);
                let sent = crate::tools::dispatch(tool, &args).await.expect(tool);
                crate::contract_tests::validate_output(tool, &sent);
                assert_eq!(sent["sent"], page_id, "{tool}");
                assert_eq!(next_intent(&mut producer).await, want, "{tool}");
            }

            let bad_key = json!({ "target": page_id, "key": "F13", "socket": socket });
            let err = call("phux_document_key", &bad_key).await.unwrap_err();
            assert!(err.0.contains("ArrowDown"), "{}", err.0);

            let classless =
                json!({ "target": format_terminal_id(&bare), "text": "x", "socket": socket });
            let err = call("phux_document_type", &classless).await.unwrap_err();
            assert_eq!(
                err.document()["error"]["code"],
                "no_control_class",
                "{}",
                err.0
            );
            assert!(err.0.contains("no control class"), "{}", err.0);

            drop(producer);
            let gone = json!({ "target": page_id, "text": "x", "socket": socket });
            let err = call("phux_document_type", &gone).await.unwrap_err();
            assert_eq!(err.document()["error"]["code"], "no_producer", "{}", err.0);

            join_after_shutdown(shutdown, server).await;
        });
    }
}
