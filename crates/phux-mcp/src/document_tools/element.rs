//! `phux_click_element`: activate one node of an accessibility tree by its
//! id or name instead of a pixel, as `phux agent tree click` does; both run
//! `phux_client::tree::element::click`.

use phux_client::selector::format_terminal_id;
use phux_client::tree::element::{self, ClickError, Clicked, FindError, Selector};
use phux_protocol::wire::frame::MAX_DOCUMENT_INPUT_TEXT;
use serde_json::{Value, json};

use super::SCHEMA_VERSION;
use crate::cli_tools::{schema, string_schema};
use crate::tools::{ToolError, strict_object};

pub(super) const NAME: &str = "phux_click_element";

pub(super) fn schema_entry() -> Value {
    let text = |what: &str| json!({ "type": "string", "minLength": 1, "maxLength": MAX_DOCUMENT_INPUT_TEXT, "description": what });
    schema(
        NAME,
        "Click one element of an accessibility tree by node, not pixel, as `phux agent tree click`: name it by `node` (the `id` phux_tree_snapshot shows on each node) or by `name` (title or description, ignoring case; an exact name beats a substring) with an optional `role`. Several equal matches are an error listing their ids. The tree's producer presses the element (macOS AXPress, AT-SPI DoAction, a DevTools click on a page element); when the tree refuses input, or with `pixel`, the element's centre is clicked on the window surface beside it. Confirm with phux_tree_wait. Needs the owner socket or an `input@<control>` grant for the tree (and the surface on the pixel path). Not read-only, and destructive. Returns the node and `path` semantic or pixel.",
        json!({
            "target": { "type": "string", "minLength": 1, "maxLength": 4096, "description": "The accessibility tree, by the `@N` id phux_documents lists." },
            "node": text("The node's `id`."),
            "name": text("The node's name."),
            "role": text("With `name`, only nodes of this role (`AXButton`, `push button`, `button`)."),
            "pixel": { "type": "boolean", "description": "Click the centre on the window surface, skipping the producer's action." },
            "socket": string_schema(),
        }),
        &["target"],
    )
}

pub(super) async fn call(args: &Value) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "node", "name", "role", "pixel", "socket"],
        &["target"],
    )?;
    let str_arg = |key: &str| args.get(key).and_then(Value::as_str);
    let selector = match (str_arg("node"), str_arg("name")) {
        (Some(id), None) if str_arg("role").is_none() => Selector::Id(id),
        (None, Some(name)) => Selector::Name {
            name,
            role: str_arg("role"),
        },
        _ => {
            return Err(ToolError::new(
                "name exactly one of `node` or `name` (`role` only with `name`)",
            ));
        }
    };
    let pixel = args.get("pixel").and_then(Value::as_bool).unwrap_or(false);
    let (socket, id) = crate::resource_tools::target(args).await?;
    let (node, how) = element::click(&socket, &id, selector, pixel)
        .await
        .map_err(|err| click_error(&err))?;
    let mut out = json!({
        "schema_version": SCHEMA_VERSION,
        "node": node.id,
        "role": node.role,
        "name": node.name,
        "path": "semantic",
    });
    if let Clicked::Pixel { surface, x, y } = how {
        out["path"] = json!("pixel");
        out["surface"] = json!(format_terminal_id(&surface));
        out["pixel"] = json!([x, y]);
    }
    Ok(out)
}

fn click_error(err: &ClickError) -> ToolError {
    let candidates = match err {
        ClickError::Find(FindError::Ambiguous(many)) => many
            .iter()
            .map(|e| json!({ "node": e.id, "role": e.role, "name": e.name }))
            .collect(),
        _ => Vec::new(),
    };
    let document = json!({
        "schema_version": SCHEMA_VERSION,
        "error": { "message": err.to_string() },
        "candidates": candidates,
    });
    ToolError::with_document(err.to_string(), document)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn the_schema_names_a_tree_and_exactly_one_selector_is_checked_first() {
        let entry = schema_entry();
        assert_eq!(entry["name"], NAME);
        let props = &entry["inputSchema"]["properties"];
        for key in ["target", "node", "name", "role", "pixel", "socket"] {
            assert!(props.get(key).is_some(), "{key}");
        }
        assert_eq!(entry["inputSchema"]["required"], json!(["target"]));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for args in [
            json!({ "target": "@1" }),
            json!({ "target": "@1", "node": "0", "name": "Save" }),
            json!({ "target": "@1", "node": "0", "role": "AXButton" }),
        ] {
            let err = runtime.block_on(call(&args)).unwrap_err();
            assert!(err.to_string().contains("exactly one"), "{err}");
        }
        assert!(
            runtime
                .block_on(call(&json!({ "target": "@1", "x": 1 })))
                .is_err()
        );
    }

    #[test]
    fn an_ambiguous_name_lists_its_candidates() {
        let many = vec![element::Element {
            id: "0.1".into(),
            role: "AXButton".into(),
            name: "Save".into(),
            frame: None,
            window: None,
        }];
        let err = click_error(&ClickError::Find(FindError::Ambiguous(many)));
        let document = err.document();
        assert_eq!(document["candidates"][0]["node"], "0.1");
    }

    /// A PNG header naming a `w` x `h` image: all a surface checks.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec();
        png.extend_from_slice(&w.to_be_bytes());
        png.extend_from_slice(&h.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    async fn next(
        producer: &mut phux_client::attach::connection::Connection,
    ) -> (String, phux_client::tree::DocumentInput) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(intent) = phux_client::tree::intent(&producer.recv().await.unwrap()) {
                    return (format_terminal_id(&intent.resource), intent.input);
                }
            }
        })
        .await
        .expect("an intent reaches the producer")
    }

    #[test]
    fn a_click_asks_the_producer_and_falls_back_to_the_window_surface() {
        use std::time::Duration;

        use phux_client::attach::connection::Connection;
        use phux_client::tree::{self, DocumentInput, Kind};
        use phux_protocol::wire::frame::ControlClass;
        use phux_server_testkit::{
            SOCKET_CONNECT_DEADLINE, join_after_shutdown, run_local, spawn_server_with,
            wait_for_socket,
        };

        run_local(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("element.sock");
            let (shutdown, server) = spawn_server_with(socket.clone(), Some("main"), |_| {});
            drop(wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await);
            let view = phux_client::state::get_state(&socket).await.unwrap();
            let pane = phux_client::resource::terminals(&view.into_snapshot_ignoring_degradation())
                .next()
                .unwrap()
                .id
                .clone();
            let window = Some(ControlClass::Window);
            let document = json!({"role": "AXApplication", "id": "0", "children": [
                {"role": "AXWindow", "id": "0.0", "frame": [100, 50, 800, 600], "children": [
                    {"role": "AXButton", "id": "0.0.0", "description": "Save", "frame": [120, 70, 60, 20]}
                ]}
            ]});
            let mut producer = Connection::connect(&socket).await.unwrap();
            let tree_id = tree::open_kind(&mut producer, Kind::Tree, &pane, "TextEdit", window)
                .await
                .unwrap()
                .resource;
            let surface = tree::open_kind(&mut producer, Kind::Surface, &pane, "TextEdit", window)
                .await
                .unwrap()
                .resource;
            let bytes = serde_json::to_vec(&document).unwrap();
            tree::publish(&mut producer, &tree_id, bytes.clone())
                .await
                .unwrap();
            tree::publish(&mut producer, &surface, png(1600, 1200))
                .await
                .unwrap();
            let (tree_at, surface_at) =
                (format_terminal_id(&tree_id), format_terminal_id(&surface));

            let args = json!({ "target": tree_at, "name": "save", "socket": socket });
            let clicked = crate::tools::dispatch(NAME, &args).await.unwrap();
            crate::contract_tests::validate_output(NAME, &clicked);
            assert_eq!(
                (clicked["node"].as_str(), clicked["path"].as_str()),
                (Some("0.0.0"), Some("semantic"))
            );
            let activate = DocumentInput::Activate {
                node: "0.0.0".into(),
            };
            assert_eq!(next(&mut producer).await, (tree_at.clone(), activate));

            // Retina frames: two pixels per point, from the window's origin.
            let args =
                json!({ "target": tree_at, "node": "0.0.0", "pixel": true, "socket": socket });
            let clicked = crate::tools::dispatch(NAME, &args).await.unwrap();
            crate::contract_tests::validate_output(NAME, &clicked);
            assert_eq!(clicked["pixel"], json!([100, 60]));
            let click = DocumentInput::Click { x: 100, y: 60 };
            assert_eq!(
                next(&mut producer).await,
                (surface_at.clone(), click.clone())
            );

            // A tree whose producer is gone falls back to the surface.
            let mut gone = Connection::connect(&socket).await.unwrap();
            let orphan = tree::open_kind(&mut gone, Kind::Tree, &pane, "TextEdit", window)
                .await
                .unwrap()
                .resource;
            tree::publish(&mut gone, &orphan, bytes).await.unwrap();
            drop(gone);
            let args =
                json!({ "target": format_terminal_id(&orphan), "node": "0.0.0", "socket": socket });
            let clicked = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let clicked = crate::tools::dispatch(NAME, &args).await.unwrap();
                    if clicked["path"] == "pixel" {
                        return clicked;
                    }
                    // The server has not seen the producer leave yet.
                    let _ = next(&mut producer).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(clicked["surface"], surface_at);
            assert_eq!(next(&mut producer).await, (surface_at, click));

            let err = call(&json!({ "target": tree_at, "node": "9", "socket": socket }))
                .await
                .unwrap_err();
            assert!(err.0.contains("no node matches"), "{}", err.0);
            join_after_shutdown(shutdown, server).await;
        });
    }
}
