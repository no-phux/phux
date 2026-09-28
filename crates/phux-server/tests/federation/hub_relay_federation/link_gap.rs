//! A satellite's scope-less `journal_gap` on the hub link's own subscription
//! must reach hub consumers as a `source_gap` per relayed Terminal (ADR-0123).
//! The overflow is real: a burst of OSC-133 marks fills the link's mailbox on
//! the satellite (while staying under the engine sink, so the satellite emits
//! no `source_gap` of its own), and a later mark flushes the gap notice.

use super::*;

#[test]
fn a_satellites_link_gap_reaches_the_hubs_consumer_as_a_source_gap() {
    phux_server_testkit::run_local(async {
        let gate = TempDir::new().unwrap();
        let release = gate.path().join("release");
        let marks = "\\033]133;C\\007\\033]133;D;0\\007".repeat(30);
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.arg("-c");
        seed.arg(format!(
            "until [ -f '{}' ]; do sleep 0.01; done; printf '{marks}'; sleep 1; \
             printf '\\033]133;C\\007'; sleep 600",
            release.display(),
        ));
        let fed = Fed::boot_with(Some(seed), None).await;
        let sat_id = fed.sat_id();
        let mut hub = fed.linked_hub().await;

        // Filtered to cwd changes so the burst cannot crowd out the notice;
        // the link itself subscribes unfiltered, and a gap bypasses filters.
        let subscribe = Command::SubscribeResourceEvents {
            terminal_id: sat_id.clone(),
            event_types: vec![phux_protocol::wire::frame::ResourceEventType::CwdChanged],
        };
        assert_eq!(
            command_via_hub(&mut hub, 50, subscribe).await.0,
            CommandResult::Ok
        );
        std::fs::write(&release, b"go").unwrap();

        let gap = next_event(&mut hub, |frame| {
            matches!(
                frame,
                FrameKind::Event {
                    event: AgentEvent::SourceGap { .. },
                    ..
                }
            )
        })
        .await;
        let FrameKind::Event {
            terminal,
            event: AgentEvent::SourceGap { dropped },
            stamp,
        } = gap
        else {
            unreachable!()
        };
        assert_eq!(
            terminal.as_ref(),
            Some(&sat_id),
            "scoped to the relayed Terminal"
        );
        assert!(dropped > 0);
        assert!(stamp.is_some(), "journaled by the hub");

        drop(hub);
        fed.shutdown().await;
    });
}
