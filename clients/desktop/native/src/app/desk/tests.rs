use super::{
    TabGeometry, command_span, display_title, held_paste_target_holds, paint_batch, pane_label,
    tab_drop_index,
};
use crate::settings::TabPosition;
use phux_client_runtime::control::{Event, PaneDescriptor};
use phux_protocol::ResourceId;
use std::time::Duration;

#[test]
fn a_confirmed_paste_goes_only_to_the_terminal_it_was_held_for() {
    let held = ResourceId::local(7);
    assert!(held_paste_target_holds(Some(&ResourceId::local(7)), &held));
    assert!(!held_paste_target_holds(Some(&ResourceId::local(8)), &held));
    assert!(!held_paste_target_holds(None, &held));
}

#[test]
fn a_finished_command_reads_its_run_time_coarsely() {
    assert_eq!(command_span(Duration::from_millis(42_900)), "42s");
    assert_eq!(command_span(Duration::from_secs(185)), "3m 5s");
    assert_eq!(command_span(Duration::from_secs(3725)), "1h 2m");
}

fn pane(name: Option<&str>, title: Option<&str>, cwd: Option<&str>) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(1),
        session_id: 1,
        session_name: "work".into(),
        cols: 80,
        rows: 24,
        title: title.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
        is_focused: false,
        key: None,
        revision: None,
        name: name.map(str::to_owned),
        activation: None,
        exec: None,
    }
}

#[test]
fn a_label_is_the_name_then_title_then_folder_then_group() {
    let group = Some("logs");
    let all = pane(Some("api"), Some("vim"), Some("/src/phux"));
    assert_eq!(pane_label(Some(&all), group), "api");
    let untitled = pane(Some(" "), Some("vim"), Some("/src/phux"));
    assert_eq!(pane_label(Some(&untitled), group), "vim");
    assert_eq!(
        pane_label(Some(&pane(None, None, Some("/src/phux/"))), group),
        "phux"
    );
    assert_eq!(pane_label(Some(&pane(None, None, None)), group), "logs");
    assert_eq!(pane_label(Some(&pane(None, None, None)), None), "shell");
    assert_eq!(pane_label(None, group), "shell");
}

#[test]
fn a_revoked_attach_says_why_without_a_server_message() {
    use phux_protocol::wire::frame::DetachReason;
    assert_eq!(
        super::detach_detail(Some(DetachReason::AuthorizationRevoked), ""),
        "this connection's authorization was revoked"
    );
    assert_eq!(
        super::detach_detail(Some(DetachReason::AuthorizationExpired), "token aged out"),
        "this connection's authorization expired: token aged out"
    );
    assert!(!super::detach_detail(None, "").is_empty());
}

#[test]
fn a_hostile_title_loses_controls_and_bidi_overrides() {
    // OSC 2 text that tries to forge a filename, smuggle an escape and a
    // newline, and run on forever.
    let hostile = format!(
        "\u{1b}]0;x\u{7}inv\u{202e}fdp.exe\u{2066}\nrm -rf ~\u{7f}{}",
        "a".repeat(4096)
    );
    let clean = display_title(&hostile);
    assert!(clean.starts_with("]0;xinvfdp.exerm -rf ~a"), "{clean:?}");
    assert!(clean.chars().all(|ch| !ch.is_control()));
    assert!(!clean.contains(['\u{202e}', '\u{2066}']));
    assert_eq!(clean.chars().count(), super::TITLE_CAP);
    let titled = pane(None, Some(&hostile), Some("/src/phux"));
    assert_eq!(pane_label(Some(&titled), None), clean);
    let blank = pane(Some("\u{1b}\u{202e}"), Some("\u{7}"), Some("/src/\u{200f}"));
    assert_eq!(pane_label(Some(&blank), Some("logs")), "logs");
    assert_eq!(display_title("مرحبا vim"), "مرحبا vim", "RTL text stays");
}

#[test]
fn a_dragged_tab_lands_after_the_last_center_left_of_the_pointer() {
    let centers = [50., 150., 250., 350.];
    // Tab 0 dragged between tabs 2 and 3 lands at index 2.
    assert_eq!(tab_drop_index(0, &centers, 300.), 2);
    // Tab 3 dragged between tabs 0 and 1 lands at index 1.
    assert_eq!(tab_drop_index(3, &centers, 100.), 1);
    // Past every center: the end. Before every center: the front.
    assert_eq!(tab_drop_index(1, &centers, 400.), 3);
    assert_eq!(tab_drop_index(2, &centers, 10.), 0);
    // A small move inside its own slot stays put.
    assert_eq!(tab_drop_index(1, &centers, 160.), 1);
}

#[test]
fn tab_reordering_uses_the_painted_axis_and_rejects_stale_geometry() {
    let mut geometry = TabGeometry::default();
    for position in [TabPosition::Top, TabPosition::Bottom, TabPosition::Side] {
        geometry.prepare(position, 3);
        // Before the new strip has painted, no drop may reorder the workspace.
        assert_eq!(geometry.drop_index(1, position, 500., 500.), 1);
        for index in 0..3 {
            // Nonzero origins model the titlebar/rail and a scrolled strip.
            let bounds = gpui::Bounds::new(
                gpui::point(
                    gpui::px(200. + index as f32 * 100.),
                    gpui::px(-40. + index as f32 * 40.),
                ),
                gpui::size(gpui::px(80.), gpui::px(32.)),
            );
            geometry.record(index, position, bounds);
        }
        match position {
            TabPosition::Top | TabPosition::Bottom => {
                assert_eq!(geometry.drop_index(0, position, 500., -1000.), 2);
                assert_eq!(geometry.drop_index(2, position, 220., 1000.), 0);
            }
            TabPosition::Side => {
                assert_eq!(geometry.drop_index(0, position, -1000., 100.), 2);
                assert_eq!(geometry.drop_index(2, position, 1000., -30.), 0);
            }
        }
        let other = if position == TabPosition::Top {
            TabPosition::Side
        } else {
            TabPosition::Top
        };
        assert_eq!(geometry.drop_index(1, other, 1000., 1000.), 1);
    }
    assert_eq!(tab_drop_index(2, &[], 10.), 2);
}

#[test]
fn repeated_agent_metadata_rides_the_paint_path() {
    let output = Event::TerminalChanged {
        terminal_id: ResourceId::local(1),
    };
    let metadata = Event::AgentMetadata {
        terminal_id: ResourceId::local(1),
        value: None,
    };
    assert!(paint_batch(std::slice::from_ref(&output), false));
    assert!(paint_batch(&[output.clone(), metadata.clone()], false));
    assert!(!paint_batch(&[output, metadata], true));
    assert!(!paint_batch(&[], false));
    assert!(!paint_batch(&[Event::TopologyChanged], false));
}
