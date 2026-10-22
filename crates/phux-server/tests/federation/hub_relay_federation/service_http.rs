//! `SERVICE_HTTP_*` naming a satellite runs on the satellite; the hub maps
//! the satellite's exchange to one of its own.

use super::*;
use phux_protocol::service_http::ServiceHttpRequest;
use phux_protocol::wire::frame::ErrorCode;

fn open_on_sat(service: &str, target: &str) -> Command {
    Command::ServiceHttpOpen {
        request: Box::new(ServiceHttpRequest {
            host: Some("sat".to_owned()),
            service: service.to_owned(),
            method: "GET".to_owned(),
            target: target.to_owned(),
            headers: Vec::new(),
            body: Vec::new(),
        }),
    }
}

fn code(result: &CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn the_satellite_answers_and_the_hub_owns_the_exchange_ids() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;

        let (unknown, _) = command_via_hub(&mut hub, 1, open_on_sat("nginx", "/")).await;
        assert_eq!(
            code(&unknown),
            ErrorCode::NotFound,
            "the satellite refused it"
        );
        let read = Command::ServiceHttpRead {
            exchange: 1,
            max_bytes: 1,
        };
        let (unmapped, _) = command_via_hub(&mut hub, 2, read).await;
        assert_eq!(code(&unmapped), ErrorCode::NotFound);

        drop(hub);
        fed.shutdown().await;
    });
}

/// Through the hub to the satellite host's real `OpenCode`.
#[test]
#[ignore = "needs the host's OpenCode (`opencode service status`)"]
fn live_opencode_on_a_satellite_answers_through_the_hub() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;

        let (opened, _) = command_via_hub(&mut hub, 1, open_on_sat("opencode", "/api/info")).await;
        let CommandResult::OkWith(CommandValue::ServiceHttpHead(head)) = opened else {
            panic!("open via the hub: {opened:?}");
        };
        assert_eq!(head.status, 200);
        let mut body = Vec::new();
        for request_id in 2.. {
            let read = Command::ServiceHttpRead {
                exchange: head.exchange,
                max_bytes: 64 * 1024,
            };
            let (result, _) = command_via_hub(&mut hub, request_id, read).await;
            let CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk)) = result else {
                panic!("read via the hub: {result:?}");
            };
            body.extend(chunk.bytes);
            if chunk.end {
                break;
            }
        }
        let info: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(info["version"].is_string(), "{info}");

        let close = Command::ServiceHttpClose {
            exchange: head.exchange,
        };
        let (closed, _) = command_via_hub(&mut hub, 100, close).await;
        assert_eq!(
            closed,
            CommandResult::Ok,
            "closing an ended exchange succeeds"
        );

        drop(hub);
        fed.shutdown().await;
    });
}
