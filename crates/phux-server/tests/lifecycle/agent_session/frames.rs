//! Commands aimed at the wrong resource kind. Reply-less frames are covered
//! in `resource_frames`.

use super::*;

#[test]
fn terminal_facet_commands_refuse_an_agent_session_and_vice_versa() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let screen = Command::GetScreen {
            terminal_id: session.clone(),
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        let signal = Command::SignalTerminal {
            terminal_id: session,
            signal: TerminalSignal::Interrupt,
            operation_id: None,
        };
        let append = Command::AppendResourceOutput {
            terminal_id: parent,
            bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
        };
        for (request_id, wrong) in (10..).zip([screen, signal, append]) {
            let result = command(&mut owner, request_id, wrong).await;
            assert_command_error(&result, ErrorCode::WrongResourceKind);
        }
        drop(owner);
        server.stop().await;
    });
}
