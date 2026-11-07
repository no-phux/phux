//! The document tools: read an `AccessibilityTree` or a `PixelSurface`
//! (`docs/resource-kinds.md`) the way `phux_snapshot` reads a pane. A tree
//! snapshot is the document `phux agent tree snapshot --json` prints; a
//! surface snapshot carries the PNG as MCP image content beside the facts
//! `phux agent surface snapshot --json` prints; a tree wait is
//! `phux agent tree follow --until` with a timeout that is a result, not an
//! error.

use std::time::Duration;

use base64::Engine as _;
use phux_client::attach::connection::Connection;
use phux_client::resource::kind_name;
use phux_client::selector::format_terminal_id;
use phux_client::tree::{self, Document, FollowEnd, TreeError, png_size};
use phux_protocol::ids::{ResourceId, ResourceKind};
use serde_json::{Value, json};

use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, socket_arg, strict_object};

/// The key a tool result carries an MCP content block under; `tool_content`
/// moves it into `content` and out of `structuredContent`.
pub(crate) const IMAGE_CONTENT_KEY: &str = "_mcp_image_content";

const SCHEMA_VERSION: u8 = 1;
const WAIT_DEFAULT_SECS: u64 = 60;
const WAIT_MAX_SECS: u64 = 3600;

pub(crate) fn schemas() -> Vec<Value> {
    let target = || json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": "The document resource, by the `@N` id phux_documents lists." });
    vec![
        schema(
            "phux_documents",
            "List the live document resources: accessibility trees (JSON) and pixel surfaces (PNG), each with its id, kind, parent pane, and name. Read them with phux_tree_snapshot, phux_tree_wait, or phux_surface_snapshot. Read-only.",
            json!({ "socket": string_schema() }),
            &[],
        ),
        schema(
            "phux_tree_snapshot",
            "Read an accessibility tree's newest JSON document and its `seq`; `seq` 0 with a null document means nothing is published yet. Reads through an attach, which the kind table counts as a binding, so it is not annotated read-only; the document `phux agent tree snapshot --json` prints.",
            json!({ "target": target(), "socket": string_schema() }),
            &["target"],
        ),
        schema(
            "phux_tree_wait",
            "Wait until an accessibility tree's document contains `until` (plain substring of the JSON text), as `phux agent tree follow --until`. Returns `outcome` matched with that document, closed when the tree ended first, or timed_out; a timeout is a result, not a tool error. Bounded: `timeout_secs` defaults to 60, at most 3600. Reads through an attach, which the kind table counts as a binding, so it is not annotated read-only.",
            json!({
                "target": target(),
                "until": { "type": "string", "minLength": 1, "maxLength": 4096 },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": WAIT_MAX_SECS },
                "socket": string_schema(),
            }),
            &["target", "until"],
        ),
        schema(
            "phux_surface_snapshot",
            "Read a pixel surface's newest image. The PNG is returned as MCP image content; the structured result carries `seq`, byte count, `width`, and `height` (null before the first publish). Reads through an attach, which the kind table counts as a binding, so it is not annotated read-only; the facts `phux agent surface snapshot --json` prints.",
            json!({ "target": target(), "socket": string_schema() }),
            &["target"],
        ),
    ]
}

pub(crate) fn owns(name: &str) -> bool {
    matches!(
        name,
        "phux_documents" | "phux_tree_snapshot" | "phux_tree_wait" | "phux_surface_snapshot"
    )
}

pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "phux_documents" => documents(args).await,
        "phux_tree_snapshot" => {
            strict_object(args, &["target", "socket"], &["target"])?;
            let (mut conn, id) = connect(args).await?;
            let document = tree::snapshot(&mut conn, &id)
                .await
                .map_err(|err| tree_error(&err))?;
            Ok(tree_json(&document))
        }
        "phux_tree_wait" => tree_wait(args).await,
        "phux_surface_snapshot" => {
            strict_object(args, &["target", "socket"], &["target"])?;
            let (mut conn, id) = connect(args).await?;
            let document = tree::snapshot(&mut conn, &id)
                .await
                .map_err(|err| tree_error(&err))?;
            Ok(surface_json(&document))
        }
        other => Err(ToolError::new(format!("unknown document tool: {other}"))),
    }
}

async fn documents(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let (snapshot, degradation) = phux_client::state::get_state(&socket_arg(args)?)
        .await?
        .into_parts();
    let documents: Vec<Value> = snapshot
        .resources
        .iter()
        .filter(|info| {
            matches!(
                info.kind,
                ResourceKind::AccessibilityTree | ResourceKind::PixelSurface
            )
        })
        .map(|info| {
            json!({
                "id": format_terminal_id(&info.id),
                "kind": kind_name(info.kind),
                "parent": info.parent.as_ref().map(format_terminal_id),
                "name": info.title,
            })
        })
        .collect();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "documents": documents,
        "unreachable": degradation.notices(),
    }))
}

async fn tree_wait(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "until", "timeout_secs", "socket"],
        &["target", "until"],
    )?;
    let until = crate::tools::required_str(args, "until")?.to_owned();
    if until.is_empty() {
        return Err(ToolError::new("`until` must not be empty"));
    }
    let secs = match args.get("timeout_secs") {
        None => WAIT_DEFAULT_SECS,
        Some(value) => value
            .as_u64()
            .filter(|secs| (1..=WAIT_MAX_SECS).contains(secs))
            .ok_or_else(|| ToolError::new("`timeout_secs` must be an integer in 1..=3600"))?,
    };
    let (mut conn, id) = connect(args).await?;
    let mut matched = None;
    let follow = tree::follow(&mut conn, &id, |document: Document| {
        let hit = String::from_utf8_lossy(&document.bytes).contains(&until);
        if hit {
            matched = Some(document);
        }
        !hit
    });
    let end = match tokio::time::timeout(Duration::from_secs(secs), follow).await {
        Ok(end) => end.map_err(|err| tree_error(&err))?,
        Err(_) => return Ok(json!({ "schema_version": SCHEMA_VERSION, "outcome": "timed_out" })),
    };
    match (matched, end) {
        (Some(document), _) => {
            let mut doc = tree_json(&document);
            doc["outcome"] = json!("matched");
            Ok(doc)
        }
        (None, FollowEnd::Closed) => {
            Ok(json!({ "schema_version": SCHEMA_VERSION, "outcome": "closed" }))
        }
        (None, end) => Err(ToolError::new(format!(
            "the tree ended ({end:?}) before a document matched"
        ))),
    }
}

/// `{ schema_version, seq, document }`, as the CLI prints a tree.
fn tree_json(document: &Document) -> Value {
    let value = serde_json::from_slice(&document.bytes).unwrap_or(Value::Null);
    json!({ "schema_version": SCHEMA_VERSION, "seq": document.seq, "document": value })
}

/// The surface facts, plus the PNG as an MCP image block when there is one.
fn surface_json(document: &Document) -> Value {
    let size = png_size(&document.bytes);
    let mut doc = json!({
        "schema_version": SCHEMA_VERSION,
        "seq": document.seq,
        "bytes": document.bytes.len(),
        "width": size.map(|s| s.0),
        "height": size.map(|s| s.1),
    });
    if size.is_some() {
        doc[IMAGE_CONTENT_KEY] = json!({
            "type": "image",
            "data": base64::engine::general_purpose::STANDARD.encode(&document.bytes),
            "mimeType": "image/png",
        });
    }
    doc
}

async fn connect(args: &Value) -> Result<(Connection, ResourceId), ToolError> {
    let (socket, id) = crate::resource_tools::target(args).await?;
    Ok((Connection::connect(&socket).await?, id))
}

fn tree_error(err: &TreeError) -> ToolError {
    ToolError::new(err.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests {
    use phux_client::tree::Kind;
    use phux_server_testkit::{
        SOCKET_CONNECT_DEADLINE, join_after_shutdown, run_local, spawn_server_with, wait_for_socket,
    };

    use super::*;

    /// A 1x1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0,
        0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44,
        0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0, 0, 0, 2, 0, 1, 0xe5, 0x27, 0xde, 0xfc, 0, 0, 0, 0,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    async fn invoke(name: &str, socket: &std::path::Path, mut args: Value) -> Value {
        args["socket"] = json!(socket);
        let mut result = call(name, &args).await.expect(name);
        let image = result.as_object_mut().unwrap().remove(IMAGE_CONTENT_KEY);
        crate::contract_tests::validate_output(name, &result);
        if let Some(image) = image {
            result[IMAGE_CONTENT_KEY] = image;
        }
        result
    }

    #[test]
    fn documents_read_back_as_json_image_content_and_a_bounded_wait() {
        run_local(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("docs.sock");
            let (shutdown, server) = spawn_server_with(socket.clone(), Some("main"), |_| {});
            drop(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
            let view = phux_client::state::get_state(&socket).await.unwrap();
            let pane = phux_client::resource::terminals(&view.into_snapshot_ignoring_degradation())
                .next()
                .unwrap()
                .id
                .clone();
            let mut producer = Connection::connect(&socket).await.unwrap();
            let tree_id = tree::open_kind(&mut producer, Kind::Tree, &pane, "smoke")
                .await
                .unwrap()
                .resource;
            let surface_id = tree::open_kind(&mut producer, Kind::Surface, &pane, "smoke")
                .await
                .unwrap()
                .resource;
            tree::publish(&mut producer, &tree_id, br#"{"title":"hello"}"#.to_vec())
                .await
                .unwrap();
            tree::publish(&mut producer, &surface_id, PNG.to_vec())
                .await
                .unwrap();
            let tree_target = json!({ "target": format_terminal_id(&tree_id) });
            let surface_target = json!({ "target": format_terminal_id(&surface_id) });

            let listed = invoke("phux_documents", &socket, json!({})).await;
            let kinds: Vec<&str> = listed["documents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|doc| doc["kind"].as_str().unwrap())
                .collect();
            assert_eq!(kinds, ["accessibility_tree", "pixel_surface"], "{listed}");

            let snap = invoke("phux_tree_snapshot", &socket, tree_target.clone()).await;
            assert_eq!(snap["document"], json!({ "title": "hello" }));

            let surface = invoke("phux_surface_snapshot", &socket, surface_target).await;
            assert_eq!(
                (surface["width"].clone(), surface["height"].clone()),
                (json!(1), json!(1))
            );
            let image = &surface[IMAGE_CONTENT_KEY];
            assert_eq!(image["type"], "image");
            assert_eq!(image["mimeType"], "image/png");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(image["data"].as_str().unwrap())
                .unwrap();
            assert_eq!(decoded, PNG);
            let content = crate::tool_content(&surface, false);
            assert_eq!(content["content"][1], *image);
            assert!(
                content["structuredContent"]
                    .get(IMAGE_CONTENT_KEY)
                    .is_none()
            );
            assert!(
                !content["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("image/png")
            );

            let mut wait = tree_target.clone();
            wait["until"] = json!("hello");
            let hit = invoke("phux_tree_wait", &socket, wait.clone()).await;
            assert_eq!(hit["outcome"], "matched");
            assert_eq!(hit["document"]["title"], "hello");
            wait["until"] = json!("never-there");
            wait["timeout_secs"] = json!(1);
            let miss = invoke("phux_tree_wait", &socket, wait).await;
            assert_eq!(miss["outcome"], "timed_out");

            drop(producer);
            join_after_shutdown(shutdown, server).await;
        });
    }
}
