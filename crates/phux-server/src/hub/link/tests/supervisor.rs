//! The link supervisor: backoff, redial, fail-closed, token rereads, keepalive, and cancellation.

use super::*;

/// A live relay handle and mailbox; the handle must stay alive or the
/// supervisor exits.
pub(super) fn relay_pair(
    host: &SatelliteHost,
) -> (
    crate::hub::relay::RelayHandle,
    crate::hub::relay::RelayMailbox,
) {
    crate::hub::relay::RelayHandle::new(host.clone())
}

pub(super) fn loopback_entry() -> HubEntry {
    entry("ws://127.0.0.1:9", None, None)
}

pub(super) fn host() -> SatelliteHost {
    SatelliteHost::new("devbox")
}

/// Poll `statuses` under paused time until `want` matches.
pub(super) async fn wait_for_status(
    statuses: &HubLinkStatuses,
    host: &SatelliteHost,
    want: impl Fn(&LinkStatus) -> bool,
) {
    for _ in 0..60_000 {
        if statuses.get(host).as_ref().is_some_and(&want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("status never matched; last = {:?}", statuses.get(host));
}

#[tokio::test(start_paused = true)]
async fn supervisor_redials_after_a_lost_connection_with_reset_backoff() {
    on_local(async {
        let (close_tx, closable) = ScriptConn::closable();
        let transport = ScriptTransport::new(vec![
            // Fail once so the backoff has a streak to reset.
            Err("boom-1".to_owned()),
            Ok(closable),
            Ok(ScriptConn::open_forever()),
        ]);
        let link = supervise(loopback_entry(), transport.clone());

        link.reach(LinkStatus::Connected).await;

        // A connection that proved stable resets the streak.
        tokio::time::sleep(LINK_STABLE_AFTER).await;
        close_tx
            .send("satellite went away".to_owned())
            .await
            .expect("send");
        link.reach(backoff(
            1,
            Duration::from_millis(500),
            "satellite went away",
        ))
        .await;
        link.reach(LinkStatus::Connected).await;
        assert_eq!(transport.calls.get(), 3);

        link.cancel.cancel();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_keeps_backing_off_when_connections_die_young() {
    on_local(async {
        // An established connection that dies at once (a refused
        // ssh auth) still grows the backoff.
        let dead = || {
            let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
            drop(tx);
            Ok(ScriptConn {
                closed: Some(rx),
                keepalive_error: None,
            })
        };
        let transport =
            ScriptTransport::new(vec![dead(), dead(), dead(), Ok(ScriptConn::open_forever())]);
        let link = supervise(loopback_entry(), transport.clone());

        for (attempt, retry_in) in [
            (1, Duration::from_millis(500)),
            (2, Duration::from_secs(1)),
            (3, Duration::from_secs(2)),
        ] {
            link.reach(backoff(attempt, retry_in, "connection closed by satellite"))
                .await;
        }
        link.reach(LinkStatus::Connected).await;
        assert_eq!(transport.calls.get(), 4);

        link.cancel.cancel();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn refused_link_fails_relay_commands_fast() {
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};

    on_local(async {
        let transport = ScriptTransport::new(vec![]);
        let link = supervise(entry("quic://devbox:8788", None, None), transport.clone());

        // Commands through a refused link fail typed, no dial.
        let result =
            tokio::time::timeout(Duration::from_secs(5), link.relay.command(Command::Upgrade))
                .await
                .expect("fail fast, not hang");
        assert!(matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                ..
            }
        ));
        assert_eq!(transport.calls.get(), 0, "refused link never dials");
        link.wait(|s| matches!(s, LinkStatus::Refused { .. })).await;
        link.cancel.cancel();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_rereads_the_token_file_every_attempt() {
    on_local(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let token_path = dir.path().join("sat.token");
        std::fs::write(&token_path, "deadbeef\n").expect("write");

        let (close_tx, closable) = ScriptConn::closable();
        let transport = ScriptTransport::new(vec![Ok(closable), Ok(ScriptConn::open_forever())]);
        let link = supervise(
            entry(
                "wss://devbox:8787",
                Some(token_path.to_str().expect("utf8 path")),
                Some("AB:CD"),
            ),
            transport.clone(),
        );

        link.reach(LinkStatus::Connected).await;

        // Rotate the token and drop the link: the redial uses the new
        // token. Wait for Backoff first so Connected is the new one.
        std::fs::write(&token_path, "c0ffee\n").expect("rotate");
        close_tx.send("rotated".to_owned()).await.expect("send");
        link.wait(
            |s| matches!(s, LinkStatus::Backoff { last_error, .. } if last_error == "rotated"),
        )
        .await;
        link.reach(LinkStatus::Connected).await;

        assert_eq!(
            *transport.seen_tokens.borrow(),
            vec![Some("deadbeef".to_owned()), Some("c0ffee".to_owned())]
        );

        link.cancel.cancel();
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn supervisor_tears_down_a_link_whose_keepalive_fails() {
    on_local(async {
        // A silent WS partition: readable forever, but keepalive
        // fails, so the link must be torn down and redialed.
        let transport = ScriptTransport::new(vec![
            Ok(ScriptConn::keepalive_fails("satellite sent nothing")),
            Ok(ScriptConn::open_forever()),
        ]);
        let link = supervise(loopback_entry(), transport.clone());

        link.wait(|s| {
            matches!(
                s,
                LinkStatus::Backoff { last_error, .. }
                    if last_error == "satellite sent nothing"
            )
        })
        .await;
        link.reach(LinkStatus::Connected).await;
        assert_eq!(transport.calls.get(), 2);

        link.cancel.cancel();
    })
    .await;
}

#[test]
fn ws_idle_error_trips_only_at_the_limit() {
    assert!(ws_idle_error(Duration::ZERO).is_none());
    assert!(
        ws_idle_error(
            LINK_IDLE_TIMEOUT
                .checked_sub(Duration::from_secs(1))
                .expect("idle timeout exceeds one second"),
        )
        .is_none()
    );
    let reason = ws_idle_error(LINK_IDLE_TIMEOUT).expect("at the limit");
    assert!(reason.contains("idle limit 30s"), "{reason}");
    assert!(ws_idle_error(LINK_IDLE_TIMEOUT * 2).is_some());
}

#[tokio::test(start_paused = true)]
async fn supervisor_stops_on_cancellation() {
    on_local(async {
        let transport = ScriptTransport::new(vec![Err("boom".to_owned())]);
        let link = supervise(loopback_entry(), transport);
        link.wait(|s| matches!(s, LinkStatus::Backoff { .. })).await;
        link.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), link.task)
            .await
            .expect("supervisor exits on cancel")
            .expect("no panic");
    })
    .await;
}
