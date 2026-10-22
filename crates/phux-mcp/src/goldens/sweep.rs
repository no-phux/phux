//! The sweep proving no in-process tool spawns the CLI.

use serde_json::{Value, json};

use crate::cli_adapter::spawn_record;
use crate::tool_table::{Exec, TOOLS};

/// Every in-process tool with minimal valid arguments against a socket
/// nothing listens on: each fails at connect (or earlier), and none may
/// reach the CLI adapter.
fn sweep_args(tool: &str) -> Value {
    let dead = "/nonexistent/phux-mcp-sweep.sock";
    let mut args = match tool {
        "phux_operation_status" => json!({ "idempotency_key": "0123456789abcdef0123456789abcdef" }),
        "phux_ls" | "phux_spawn" | "phux_agent_clear" | "phux_approvals" => json!({}),
        "phux_snapshot" | "phux_resource_methods" | "phux_resource_show" => {
            json!({ "target": "@1" })
        }
        "phux_wait" | "phux_watch" | "phux_resource_wait" => {
            json!({ "target": "@1", "timeout_secs": 1 })
        }
        "phux_send_keys" => json!({ "target": "@1", "keys": ["x"] }),
        "phux_paste" => json!({ "target": "@1", "text": "x" }),
        "phux_upload" => json!({ "target": "@1", "path": "local.bin" }),
        "phux_kill" | "phux_rm" => json!({ "target": "@1", "confirm": true }),
        "phux_detach" => json!({ "confirm": true }),
        "phux_ask" => json!({ "target": "@1", "id": "q", "question": "ok?" }),
        "phux_signal" => json!({ "target": "@1", "signal": "freeze" }),
        "phux_tag" => json!({ "action": "ls", "target": "@1" }),
        "phux_rename" => json!({ "session": "a", "new_name": "b" }),
        "phux_insert_pane" => json!({ "target": "@1", "new_pane": "@2" }),
        "phux_move_pane" => json!({ "source": "@1", "target": "@2" }),
        "phux_swap_pane" => json!({ "first": "@1", "second": "@2" }),
        "phux_agent_set" => json!({ "name": "bot" }),
        "phux_approve" => json!({ "id": "0123456789abcdef0123456789abcdef", "decision": "deny" }),
        "phux_relaunch" => json!({ "target": "@1" }),
        "phux_organize" => json!({ "offline": true }),
        "phux_plugin_action" | "phux_plugin_profile" => {
            return json!({
                "plugin_id": "none", "action_id": "none",
                "config": "/nonexistent/phux-mcp-sweep/config.toml",
            });
        }
        // Coordination tools never take `socket`, so the sweep's dead socket
        // is refused before the work store is touched.
        coord if crate::coord_tools::owns(coord) => json!({}),
        other => panic!("{other} is in-process but has no sweep arguments"),
    };
    args["socket"] = json!(dead);
    args
}

#[tokio::test]
async fn no_tool_outside_the_residue_spawns_a_subprocess() {
    let catalog = crate::tools::catalog();
    let names: Vec<&str> = catalog
        .as_array()
        .expect("catalog")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    let _ = spawn_record::take();
    for row in TOOLS.iter().filter(|row| !matches!(row.exec, Exec::Cli(_))) {
        assert!(names.contains(&row.name), "stale row {}", row.name);
        let result = crate::tools::dispatch(row.name, &sweep_args(row.name)).await;
        if let Err(err) = &result {
            assert!(
                !err.0.contains("must not spawn the phux CLI"),
                "{} reached the CLI adapter: {}",
                row.name,
                err.0
            );
        }
        assert_eq!(
            spawn_record::take(),
            Vec::<String>::new(),
            "{} spawned the CLI",
            row.name
        );
    }

    // Positive control: a residue tool does reach the adapter, so an empty
    // record above is evidence, not a recorder that never fires.
    let _ = crate::tools::dispatch(
        "phux_doctor",
        &json!({ "socket": "/nonexistent/phux-mcp-sweep.sock" }),
    )
    .await;
    assert_eq!(spawn_record::take().len(), 1, "the residue control spawned");
}
