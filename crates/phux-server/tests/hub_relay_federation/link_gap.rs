//! A satellite's `journal_gap` on the hub link's own subscription reaches
//! the hub's consumers (ADR-0123, `docs/spec/L1.md` §7.3).
//!
//! The satellite sends that gap with no Terminal, because the link holds
//! one subscription for every scope it relays. The hub must turn it into a
//! `source_gap` for each relayed Terminal rather than drop it for want of a
//! scope, or a consumer behind the hub loses events silently.
//!
//! The overflow is real, not a forged frame: the satellite's pane emits a
//! burst of OSC-133 marks that its event drain journals in one turn, which
//! fills the link's eight-slot mailbox on the satellite before the link's
//! writer can run. The burst stays under the 64-event engine sink, so the
//! satellite reports no `source_gap` of its own; a mark a second later
//! flushes the satellite's gap notice to the hub if nothing else has.
//! The hub consumer filters to cwd changes, so the notice reaches it alone.

use super::*;
use phux_server_testkit::recv_command_result;

#[test]
fn a_satellites_link_gap_reaches_the_hubs_consumer_as_a_source_gap() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let release = tmp.path().join("release");
        let marks = "\\033]133;C\\007\\033]133;D;0\\007".repeat(30);
        let mut seed = portable_pty::CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!(
            "until [ -f '{}' ]; do sleep 0.01; done; printf '{marks}'; sleep 1; \
             printf '\\033]133;C\\007'; sleep 600",
            release.display(),
        ));
        let (ws_port, sat_shutdown, sat_task) =
            spawn_satellite_runtime(tmp.path().join("sat.sock"), true, Some(seed));
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = ResourceId::satellite("sat", sat_pane);
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", ws_port)]);
        let mut hub = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        let _ = get_screen_until_ok(&mut hub, sat_pane).await;

        // Filtered to cwd changes on the hub, so the burst's command events
        // stay out of this consumer's mailbox and cannot crowd out the
        // notice; the link itself subscribes unfiltered, so the satellite
        // still overflows it. A `source_gap` bypasses every filter. The
        // subscribe's reply is the barrier: it rides the same link.
        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 50,
                command: Command::SubscribeResourceEvents {
                    terminal_id: sat_id.clone(),
                    event_types: vec![phux_protocol::wire::frame::ResourceEventType::CwdChanged],
                },
            },
        )
        .await;
        assert_eq!(
            recv_command_result(&mut hub, 50).await,
            CommandResult::Ok,
            "the filtered subscribe is accepted"
        );
        std::fs::write(&release, b"go").unwrap();

        let deadline = Instant::now() + STEP_DEADLINE;
        let gap = loop {
            assert!(
                Instant::now() < deadline,
                "the satellite's link gap never reached the hub consumer"
            );
            let (_, frame) = recv_typed(&mut hub).await;
            if let FrameKind::Event {
                terminal,
                event: AgentEvent::SourceGap { dropped },
                stamp,
            } = frame
            {
                break (terminal, dropped, stamp);
            }
        };
        let (terminal, dropped, stamp) = gap;
        assert_eq!(
            terminal.as_ref(),
            Some(&sat_id),
            "scoped to the relayed Terminal"
        );
        assert!(dropped > 0);
        assert!(stamp.is_some(), "journaled by the hub");

        drop(hub);
        let _ = hub_shutdown.send(());
        let _ = sat_shutdown.send(());
        let _ = tokio::time::timeout(STEP_DEADLINE, hub_task).await;
        let _ = tokio::time::timeout(STEP_DEADLINE, sat_task).await;
    });
}
