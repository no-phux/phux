//! `MIRROR` over the production dispatch loop
//! (`docs/applications.md#picking-a-screen-or-window`): Rune's macOS runner
//! reports a host's targets (`MIRROR_PROVIDE`, owner socket) and is told the
//! mirrors to run; a viewer lists the targets, from the runner on macOS and
//! from `phux agent surface targets` elsewhere.

use std::os::unix::fs::PermissionsExt as _;

use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, MirrorOp};
use phux_server_testkit::command;

use crate::common::Server;

const TARGETS: &str = r#"[{"target":"display","kind":"display","label":"fake"}]"#;

fn json(result: CommandResult) -> serde_json::Value {
    let CommandResult::OkWith(CommandValue::Json(text)) = result else {
        panic!("expected JSON, got {result:?}");
    };
    serde_json::from_str(&text).expect("json")
}

#[test]
fn the_runner_reports_targets_and_a_viewer_lists_them() {
    phux_server_testkit::run_local(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = dir.path().join("phux");
        let script = format!("#!/bin/sh\necho '{{\"schema_version\":1,\"targets\":{TARGETS}}}'\n");
        std::fs::write(&fake, script).expect("write");
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let server = Server::start(None, |cfg| cfg.native_host_executable = Some(fake));
        let mut client = server.connect().await;
        let provide = Command::MirrorProvide {
            targets: TARGETS.to_owned(),
        };
        let told = json(command(&mut client, 1, provide).await);
        assert_eq!(told["mirrors"], serde_json::json!([]), "nothing wanted yet");
        let listed = Command::Mirror {
            host: None,
            op: MirrorOp::Targets,
        };
        let listed = json(command(&mut client, 2, listed).await);
        assert_eq!(listed["targets"][0]["label"], "fake");
        let stop = Command::Mirror {
            host: None,
            op: MirrorOp::Stop {
                target: "display".to_owned(),
            },
        };
        assert!(matches!(
            command(&mut client, 3, stop).await,
            CommandResult::Error { .. }
        ));
        server.stop().await;
    });
}
