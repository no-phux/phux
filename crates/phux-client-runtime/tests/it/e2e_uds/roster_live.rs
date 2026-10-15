use super::*;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

async fn declaration(client: &Client, terminal: &ResourceId, value: Option<&[u8]>) {
    wait_for_event(client, "agent declaration", |event| match event {
        Event::AgentMetadata {
            terminal_id,
            value: observed,
        } if terminal_id == terminal && observed.as_deref() == value => Some(()),
        _ => None,
    })
    .await;
}

#[test]
fn browsing_discovers_agent_metadata_recovers_on_reconnect_and_hears_retraction() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let writer = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&writer, Status::Attached).await;
        let terminal = writer.topology().unwrap().panes[0].terminal_id.clone();
        declaration(&writer, &terminal, None).await;
        let record = br#"{"name":"roster-test","state":"working"}"#;
        writer.queue_frame(&FrameKind::SetMetadata {
            request_id: writer.next_request_id(),
            scope: Scope::Resource(terminal.clone()),
            key: RESOURCE_AGENT_KEY.into(),
            value: record.to_vec(),
        });
        declaration(&writer, &terminal, Some(record)).await;

        let mut browsing = options();
        browsing.control.attach = None;
        let reader = Runtime::connect(Target::uds(&socket), browsing).unwrap();
        declaration(&reader, &terminal, Some(record)).await;
        reader.resync();
        declaration(&reader, &terminal, Some(record)).await;
        writer.queue_frame(&FrameKind::DeleteMetadata {
            request_id: writer.next_request_id(),
            scope: Scope::Resource(terminal.clone()),
            key: RESOURCE_AGENT_KEY.into(),
        });
        declaration(&reader, &terminal, None).await;
        reader.close();
        writer.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}

async fn asked_level(client: &Client, terminal: &ResourceId, level: bool) {
    wait_for_event(client, "asked level", |event| match event {
        Event::AgentAskedState { terminal_id, asked }
            if terminal_id == terminal && *asked == level =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
}

#[test]
fn a_question_missed_before_connecting_is_recovered_from_the_asked_flag() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let writer = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&writer, Status::Attached).await;
        let terminal = writer.topology().unwrap().panes[0].terminal_id.clone();
        asked_level(&writer, &terminal, false).await;
        let _ = writer.send_command(phux_protocol::wire::frame::Command::ReportAsked {
            terminal_id: terminal.clone(),
            id: "q1".into(),
            question: "Proceed?".into(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        });
        asked_level(&writer, &terminal, true).await;

        // This client never saw the `asked` edge; the level still says so,
        // and says so again after a reconnect.
        let mut browsing = options();
        browsing.control.attach = None;
        let reader = Runtime::connect(Target::uds(&socket), browsing).unwrap();
        asked_level(&reader, &terminal, true).await;
        reader.resync();
        asked_level(&reader, &terminal, true).await;
        reader.close();
        writer.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
