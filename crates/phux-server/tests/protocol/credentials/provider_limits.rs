//! Admission limits apply to real provider connections, not just broker callers.

use super::common::*;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};
use phux_server_testkit::run_local;

#[test]
fn one_provider_connection_cannot_reserve_every_label() {
    run_local(async {
        let server = Server::start(300, None);
        let mut provider = server.connect("provider").await;
        let mut other = server.connect("other-provider").await;
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        for suffix in 0..32 {
            assert!(matches!(
                next(&mut provider, &format!("alias-{suffix}")).await,
                CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    ..
                }
            ));
        }
        assert_eq!(next(&mut provider, PROVIDER).await, CommandResult::Ok);
        assert_eq!(next(&mut other, "other-device").await, CommandResult::Ok);
        disconnect(provider).await;
        let mut replacement = server.connect("replacement").await;
        assert_eq!(next(&mut replacement, PROVIDER).await, CommandResult::Ok);
        assert_eq!(next(&mut other, "other-device").await, CommandResult::Ok);
        server.stop().await;
    });
}
