//! Frames and commands aimed at the wrong resource kind.

use super::*;

async fn uncorrelated_wrong_kind(stream: &mut UnixStream, what: &str) {
    wait_frame(stream, what, |frame| {
        matches!(
            frame,
            FrameKind::Error {
                code: ErrorCode::WrongResourceKind,
                ..
            }
        )
        .then_some(())
    })
    .await;
}

#[test]
fn terminal_facet_commands_refuse_an_agent_session_and_vice_versa() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;

        let key = FrameKind::InputKey {
            terminal_id: session.clone(),
            event: ascii_key('a', PhysicalKey::A),
        };
        send_frame(&mut owner, &key).await;
        uncorrelated_wrong_kind(&mut owner, "INPUT_KEY refusal").await;
        let history = FrameKind::HistoryRequest {
            terminal_id: session.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: bytes::Bytes::new(),
            max_bytes: 1024,
            max_rows: 10,
        };
        send_frame(&mut owner, &history).await;
        uncorrelated_wrong_kind(&mut owner, "HISTORY_REQUEST refusal").await;
        let resize = FrameKind::ResizeTerminal {
            terminal_id: session.clone(),
            cols: 80,
            rows: 24,
            cell_px: None,
        };
        send_frame(&mut owner, &resize).await;
        uncorrelated_wrong_kind(&mut owner, "RESIZE_TERMINAL refusal").await;

        let screen = Command::GetScreen {
            terminal_id: session.clone(),
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        assert_wrong_kind(&command(&mut owner, 10, screen).await, "GET_SCREEN");
        let signal = Command::SignalTerminal {
            terminal_id: session,
            signal: TerminalSignal::Interrupt,
            operation_id: None,
        };
        assert_wrong_kind(&command(&mut owner, 11, signal).await, "SIGNAL_TERMINAL");
        let append = Command::AppendResourceOutput {
            terminal_id: parent,
            bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
        };
        assert_wrong_kind(
            &command(&mut owner, 12, append).await,
            "APPEND on a Terminal",
        );

        drop(owner);
        server.stop().await;
    });
}
