//! `notifications.from-programs`: OSC 9 and OSC 777 become the pane's
//! `phux.note/v1` only when the server was started with `"notes"`.

use phux_config::FromPrograms;
use phux_protocol::wire::frame::RESOURCE_NOTE_KEY;

use super::*;

fn notifications() -> Vec<u8> {
    b"\x1b]9;formatted 3 files\x07\x1b]777;notify;build;failed\x1b\\".to_vec()
}

#[test]
fn osc_9_and_777_become_the_newest_note_when_enabled() {
    run_local(async {
        let stages = [fenced(notifications(), 0)];
        let mut pane =
            Pane::start_with(&stages, false, "generic-status", FromPrograms::Notes).await;
        pane.stage(0).await;
        // The burst lands as its last word.
        let note = pane
            .wait_value(RESOURCE_NOTE_KEY, |value| {
                value.as_ref().is_some_and(|v| v["title"] == "build")
            })
            .await
            .expect("note");
        assert_eq!(
            (&note["level"], &note["body"]),
            (&json!("info"), &json!("failed"))
        );
        pane.stop().await;
    });
}

#[test]
fn osc_9_and_777_stay_ignored_by_default() {
    run_local(async {
        let mut pane = Pane::start(&[fenced(notifications(), 0)], false).await;
        pane.stage(0).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(pane.get(RESOURCE_NOTE_KEY).await, None);
        pane.stop().await;
    });
}
