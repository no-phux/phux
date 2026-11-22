//! `SERVICE_HTTP_*` commands and replies: decoder bounds. Their round trips
//! run over the wire-schema sample corpus
//! (`src/wire/decode/wire_schema/roundtrip_tests.rs`).

use super::{BytesMut, Command, CommandResult, CommandValue, DecodeError, FrameKind};
use phux_protocol::service_http::{
    MAX_SERVICE_HTTP_BODY_BYTES, MAX_SERVICE_HTTP_HEAD_BYTES, MAX_SERVICE_HTTP_HEADERS,
    MAX_SERVICE_HTTP_READ_BYTES, ServiceHttpChunk, ServiceHttpRequest,
};

fn request() -> ServiceHttpRequest {
    ServiceHttpRequest {
        host: Some("mini".to_owned()),
        service: "opencode".to_owned(),
        method: "POST".to_owned(),
        target: "/api/session?directory=%2Ftmp".to_owned(),
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("accept".to_owned(), "text/event-stream".to_owned()),
        ],
        body: br#"{"title":"x"}"#.to_vec(),
    }
}

fn decoded(frame: &FrameKind) -> Result<FrameKind, DecodeError> {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    FrameKind::decode(&encoded).map(|(decoded, tail)| {
        assert!(tail.is_empty());
        decoded
    })
}

fn open(request: ServiceHttpRequest) -> FrameKind {
    FrameKind::Command {
        request_id: 5,
        command: Command::ServiceHttpOpen {
            request: Box::new(request),
        },
    }
}

const fn reply(value: CommandValue) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 5,
        result: CommandResult::OkWith(value),
    }
}

#[test]
fn service_http_decoders_refuse_what_exceeds_the_bounds() {
    let mut body = request();
    body.body = vec![0; MAX_SERVICE_HTTP_BODY_BYTES + 1];
    let mut headers = request();
    headers.headers = vec![("a".to_owned(), "b".to_owned()); MAX_SERVICE_HTTP_HEADERS + 1];
    let mut head_bytes = request();
    head_bytes.target = format!("/{}", "x".repeat(MAX_SERVICE_HTTP_HEAD_BYTES));
    let over_read = FrameKind::Command {
        request_id: 1,
        command: Command::ServiceHttpRead {
            exchange: 1,
            max_bytes: u32::try_from(MAX_SERVICE_HTTP_READ_BYTES + 1).unwrap(),
        },
    };
    let over_chunk = reply(CommandValue::ServiceHttpChunk(ServiceHttpChunk {
        bytes: vec![0; MAX_SERVICE_HTTP_READ_BYTES + 1],
        end: true,
    }));
    for frame in [
        open(body),
        open(headers),
        open(head_bytes),
        over_read,
        over_chunk,
    ] {
        assert_eq!(
            decoded(&frame).unwrap_err(),
            DecodeError::ServiceHttpLimitExceeded
        );
    }
}
