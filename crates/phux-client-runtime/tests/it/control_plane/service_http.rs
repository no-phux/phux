use super::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use phux_client_runtime::control::{MAX_SERVICE_HTTP_IN_FLIGHT, ServiceHttpError};
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};

fn plane_with_service_http() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps.with_features_ext(ServerFeatureExtSet::with(&[
            ServerFeatureExt::HostServiceHttp,
        ]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    plane
}

fn info() -> ServiceHttpRequest {
    ServiceHttpRequest {
        host: None,
        service: "opencode".to_owned(),
        method: "GET".to_owned(),
        target: "/api/info".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

/// The value a resolved call holds; panics while it is still pending.
fn ready<T>(call: impl Future<Output = T>) -> T {
    let mut call = pin!(call);
    match call.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("the call is still pending"),
    }
}

fn sent_request_id(plane: &mut ControlPlane) -> (u32, Command) {
    match decode(&plane.take_outbound()[0]) {
        FrameKind::Command {
            request_id,
            command,
        } => (request_id, command),
        other => panic!("expected COMMAND, got {other:?}"),
    }
}

#[test]
fn an_older_server_is_never_asked() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    assert_eq!(
        plane.service_http_open(info()).unwrap_err(),
        ServiceHttpError::NotConnected
    );
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let _ = plane.take_outbound();
    assert_eq!(
        plane.service_http_open(info()).unwrap_err(),
        ServiceHttpError::Unsupported
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn open_read_and_close_resolve_their_calls() {
    let mut plane = plane_with_service_http();
    let open = plane.service_http_open(info()).unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    assert!(matches!(command, Command::ServiceHttpOpen { request } if *request == info()));
    let head = ServiceHttpHead {
        exchange: 9,
        status: 200,
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
    };
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::ServiceHttpHead(head.clone())),
        })
        .unwrap();
    assert_eq!(ready(open).unwrap(), head);

    let read = plane.service_http_read(9, 1024).unwrap();
    let (request_id, command) = sent_request_id(&mut plane);
    assert!(matches!(
        command,
        Command::ServiceHttpRead {
            exchange: 9,
            max_bytes: 1024
        }
    ));
    let chunk = ServiceHttpChunk {
        bytes: b"{}".to_vec(),
        end: true,
    };
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk.clone())),
        })
        .unwrap();
    assert_eq!(ready(read).unwrap(), chunk);

    let close = plane.service_http_close(9).unwrap();
    let (request_id, _) = sent_request_id(&mut plane);
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::Unavailable,
                message: "OpenCode is not installed".to_owned(),
            },
        })
        .unwrap();
    assert!(matches!(
        ready(close),
        Err(ServiceHttpError::Refused {
            code: ErrorCode::Unavailable,
            ..
        })
    ));
}

#[test]
fn malformed_requests_send_nothing() {
    let mut plane = plane_with_service_http();
    let mut huge = info();
    huge.body = vec![0; phux_protocol::service_http::MAX_SERVICE_HTTP_BODY_BYTES + 1];
    assert!(matches!(
        plane.service_http_open(huge),
        Err(ServiceHttpError::Invalid(_))
    ));
    assert!(matches!(
        plane.service_http_read(1, 0),
        Err(ServiceHttpError::Invalid(_))
    ));
    let over = u32::try_from(phux_protocol::service_http::MAX_SERVICE_HTTP_READ_BYTES + 1).unwrap();
    assert!(matches!(
        plane.service_http_read(1, over),
        Err(ServiceHttpError::Invalid(_))
    ));
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn calls_are_bounded_and_a_disconnect_resolves_them() {
    let mut plane = plane_with_service_http();
    let calls: Vec<_> = (0..MAX_SERVICE_HTTP_IN_FLIGHT)
        .map(|_| plane.service_http_read(1, 1).unwrap())
        .collect();
    assert_eq!(
        plane.service_http_read(1, 1).unwrap_err(),
        ServiceHttpError::Busy
    );
    plane.connection_lost(None);
    for call in calls {
        assert_eq!(ready(call).unwrap_err(), ServiceHttpError::Disconnected);
    }
    assert_eq!(
        plane.service_http_read(1, 1).unwrap_err(),
        ServiceHttpError::NotConnected
    );
}

#[test]
fn a_dropped_open_closes_the_exchange_it_was_answered_with() {
    let mut plane = plane_with_service_http();
    drop(plane.service_http_open(info()).unwrap());
    let (request_id, _) = sent_request_id(&mut plane);
    let head = ServiceHttpHead {
        exchange: 42,
        status: 200,
        headers: Vec::new(),
    };
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::ServiceHttpHead(head)),
        })
        .unwrap();
    let (close_id, command) = sent_request_id(&mut plane);
    assert!(matches!(
        command,
        Command::ServiceHttpClose { exchange: 42 }
    ));
    plane
        .feed(FrameKind::CommandResult {
            request_id: close_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    assert!(
        plane.take_outbound().is_empty(),
        "the close's answer resolves nothing"
    );

    // A dropped read needs no cleanup: the caller still owns the exchange.
    drop(plane.service_http_read(42, 1).unwrap());
    let (request_id, _) = sent_request_id(&mut plane);
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::ServiceHttpChunk(ServiceHttpChunk {
                bytes: b"x".to_vec(),
                end: false,
            })),
        })
        .unwrap();
    assert!(plane.take_outbound().is_empty());
}
