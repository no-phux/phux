use super::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use phux_client_runtime::control::MirrorError;
use phux_protocol::wire::frame::MirrorOp;

fn ready<T>(call: impl Future<Output = T>) -> T {
    match pin!(call)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the call is still pending"),
    }
}

fn plane(features: &[ServerFeatureExt]) -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps.with_features_ext(ServerFeatureExtSet::with(features));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    plane
}

#[test]
fn a_mirror_request_is_refused_by_an_older_server_and_answered_by_a_newer() {
    let mut older = plane(&[]);
    assert_eq!(
        older.mirror(None, MirrorOp::Targets).unwrap_err(),
        MirrorError::Unsupported
    );
    assert!(older.take_outbound().is_empty());

    let mut plane = plane(&[ServerFeatureExt::Mirrors]);
    let start = MirrorOp::Start {
        target: "display".to_owned(),
        jpeg: false,
    };
    let call = plane.mirror(Some("box".to_owned()), start.clone()).unwrap();
    let FrameKind::Command {
        request_id,
        command: Command::Mirror { host, op },
    } = decode(&plane.take_outbound()[0])
    else {
        panic!("expected MIRROR");
    };
    assert_eq!((host.as_deref(), op), (Some("box"), start));
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Json(
                r#"{"session":"mirror-display","surface":4}"#.to_owned(),
            )),
        })
        .unwrap();
    assert_eq!(ready(call).unwrap()["surface"], 4);

    let refused = plane.mirror(None, MirrorOp::Targets).unwrap();
    let FrameKind::Command { request_id, .. } = decode(&plane.take_outbound()[0]) else {
        panic!("expected MIRROR");
    };
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::Unavailable,
                message: "open Rune".to_owned(),
            },
        })
        .unwrap();
    assert!(matches!(
        ready(refused),
        Err(MirrorError::Refused {
            code: ErrorCode::Unavailable,
            ..
        })
    ));
}
