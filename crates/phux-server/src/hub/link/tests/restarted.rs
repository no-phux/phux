//! A link that comes back to another satellite process.

use super::*;
use crate::hub::link::session::run_relay_session;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, StateScope};

/// What was queued for the old process is refused, never forwarded: its
/// terminal ids name other panes in the new one.
#[tokio::test(start_paused = true)]
async fn requests_queued_for_the_old_process_are_not_forwarded() {
    on_local(async {
        let host = host();
        let (relay, mailbox) = relay_pair(&host);
        let mut old =
            crate::hub::relay::RelaySession::new(host.clone(), BootstrapLimits::default());
        old.set_incarnation(Some([1; 16]));
        let mut resume = old.teardown("lost");

        let queued = relay.clone();
        let reply = tokio::task::spawn_local(async move {
            let scope = StateScope::Server;
            queued.command(Command::GetState { scope }).await
        });
        tokio::task::yield_now().await;
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let session = tokio::task::spawn_local(async move {
            let mut mailbox = mailbox;
            run_relay_session(
                &host,
                ScriptConn::open_forever(),
                &mut mailbox.requests,
                &mut mailbox.unsubscribes,
                &mut resume,
                &cancel,
                None,
                &crate::hub::operation_fence::OperationFence::default(),
                mailbox.resource_kinds,
            )
            .await
        });
        let CommandResult::Error { code, message } = reply.await.expect("reply") else {
            panic!("queued for the old process, answered by the new one");
        };
        assert_eq!(code, ErrorCode::SatelliteUnreachable);
        assert!(message.contains("restarted"), "{message}");
        stop.cancel();
        assert!(session.await.expect("session").is_none());
    })
    .await;
}
