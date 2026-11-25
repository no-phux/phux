//! Close acknowledgement, partial fleet visibility, and uncertain operation contracts.

use phux_client::testkit::{self, ScriptSpec};
use serde_json::json;

use super::{org_fixture, org_spec, state};

/// A kill that hits under a partial fleet view still succeeds (the CLI
/// warns and continues). The shared builder is what prints that warning.
#[tokio::test]
async fn kill_under_a_partial_view_still_kills() {
    const NOTICE: &str = "satellite build-box is unreachable: link is down";
    let dir = tempfile::tempdir().expect("temp dir");
    let spec = || ScriptSpec::new().state(state()).degradation_notice(NOTICE);
    let (socket, server) = testkit::spawn_serve_every(dir.path(), spec);
    let result = crate::tools::dispatch(
        "phux_kill",
        &json!({
            "target": "@2",
            "confirm": true,
            "socket": socket.to_string_lossy(),
        }),
    )
    .await;
    server.abort();
    let value = result.expect("partial-view kill");
    assert_eq!(value["killed"], json!(true));
    assert_eq!(value["target"], "@2");
}
#[tokio::test]
async fn kill_unknown_projects_original_wire_operation_and_target_instead_of_success() {
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
    let dir = tempfile::tempdir().expect("tempdir");
    let spec = org_spec(org_fixture().0).mutation_reply(CommandResult::Error {
        code: ErrorCode::Unknown(65535),
        message: "unreadable close outcome".into(),
    });
    let (socket, server) = testkit::serve_one(dir.path(), spec);
    let error = crate::tools::dispatch(
        "phux_kill",
        &json!({
            "target": "work", "confirm": true, "socket": socket.to_string_lossy(),
        }),
    )
    .await
    .expect_err("unknown is not killed=true");
    let document = error.document();
    let frames = server.await.expect("server");
    let operation = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                command: Command::SessionKill { operation, .. },
                ..
            } => Some(operation.to_hex()),
            _ => None,
        })
        .expect("original close");
    assert_eq!(document["error"]["code"], "outcome_unknown");
    assert_eq!(document["error"]["target"], "work");
    assert_eq!(document["error"]["recovery"]["operation_id"], operation);
    assert_eq!(
        document["error"]["recovery"]["scope"]["kind"],
        "organization_authority"
    );
    assert_eq!(document["error"]["recovery"]["effect_certainty"], "unknown");
    assert!(document.get("killed").is_none());
}
