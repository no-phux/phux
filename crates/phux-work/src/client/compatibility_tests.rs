//! Upgrade handling against legacy keepers.

#![allow(clippy::unwrap_used, reason = "test assertions")]
use super::*;

#[test]
fn versioned_errors_and_a_used_upgrade_budget_never_trigger_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let client = Local {
        endpoint: Endpoint::new(dir.path(), dir.path()),
        launcher: None,
    };
    for (keeper, allow_upgrade) in [(Some(BUILD.to_owned()), true), (None, false)] {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let fault = Response::Fault(Fault::BadRequest {
            message: "invalid payload".into(),
            keeper,
        });
        let reply = serde_json::to_string(&fault).unwrap() + "\n";
        std::io::Write::write_all(&mut peer, reply.as_bytes()).unwrap();
        let line = encode_request(Request::List(ListFilter {
            repo: None,
            archived: false,
        }))
        .unwrap();
        // The endpoint has no listener. Any compatibility probe would fail,
        // rather than returning this unchanged parser error.
        let response = client.compatible_exchange(&stream, &line, allow_upgrade, prompt());
        assert!(matches!(response, Err(Error::Keeper(message)) if message == "invalid payload"));
    }
}
