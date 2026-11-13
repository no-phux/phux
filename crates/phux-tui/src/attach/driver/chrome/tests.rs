//! Tests for the window chrome feeds.

use super::*;
use crate::attach::pane_state::{clear_attention_on_input, published_test_state};

mod agent_entries;
mod no_peers;
mod window_infos;

use no_peers::no_peers;

/// ADR-0124: a retained exited pane keeps its replica, gets an exit
/// badge, and refuses input.
#[test]
fn a_retained_pane_renders_its_last_grid_with_an_exit_mark_and_refuses_input() {
    use crate::attach::pane_state::{ExitMark, pane_exited, published_terminal};
    let id = ResourceId::local(1);
    let (kernel, _effects, mut panes) = published_test_state(&[(&id, 20, 4, b"final screen\r\n")]);
    assert!(!pane_exited(&panes, &id), "a live pane takes input");
    panes.get_mut(&id).expect("slot").exited = Some(ExitMark {
        status: Some(3),
        signal: None,
    });
    assert!(
        published_terminal(&kernel, &id).is_some(),
        "the last grid is still published for the renderer"
    );
    assert_eq!(
        supervisory_badge(&panes, Some(&id), Some(ClientId::new(7))).as_deref(),
        Some("[ exited 3 ]")
    );
    assert!(pane_exited(&panes, &id), "input to it is refused");
    let signalled = ExitMark {
        status: None,
        signal: Some(9),
    };
    assert_eq!(signalled.label(), "exited signal 9");
    assert_eq!(signalled.compact(), "sig9");
    let unknown = ExitMark {
        status: None,
        signal: None,
    };
    assert_eq!(unknown.label(), "exited");
    assert_eq!(unknown.compact(), "");
}

#[test]
fn supervisory_badge_formats_every_state() {
    let me = ClientId::new(7);
    let other = ClientId::new(9);
    assert_eq!(
        format_supervisory_badge(false, None, Some(me), None, false),
        None
    );
    assert_eq!(
        format_supervisory_badge(true, None, Some(me), None, false).as_deref(),
        Some(" frozen ")
    );
    assert_eq!(
        format_supervisory_badge(false, Some(me), Some(me), None, false).as_deref(),
        Some(" wheel ")
    );
    assert_eq!(
        format_supervisory_badge(false, Some(other), Some(me), None, false).as_deref(),
        Some(" wheel:other ")
    );
    assert_eq!(
        format_supervisory_badge(true, Some(other), Some(me), None, false).as_deref(),
        Some(" frozen wheel:other ")
    );
    // No own id yet (pre-ATTACHED): a holder is someone else, never "you".
    assert_eq!(
        format_supervisory_badge(false, Some(me), None, None, false).as_deref(),
        Some(" wheel:other ")
    );
    // A satellite pane badges its host on the status bar,
    // beside any lease or brake already shown.
    assert_eq!(
        format_supervisory_badge(false, None, Some(me), Some("devbox"), false).as_deref(),
        Some(" devbox ")
    );
    assert_eq!(
        format_supervisory_badge(true, None, Some(me), Some("devbox"), false).as_deref(),
        Some(" devbox frozen ")
    );
    // Lost progressive history is a persistent token, last in the badge.
    assert_eq!(
        format_supervisory_badge(false, None, Some(me), None, true).as_deref(),
        Some(" no-scrollback ")
    );
    assert_eq!(
        format_supervisory_badge(true, Some(me), Some(me), Some("devbox"), true).as_deref(),
        Some(" devbox frozen wheel no-scrollback ")
    );
}

/// A pane whose progressive history is unavailable keeps a badge after
/// the transient notice expires; an exited pane's mark still wins.
#[test]
fn a_history_degraded_pane_badges_no_scrollback_on_the_status_bar() {
    use crate::attach::pane_state::ExitMark;
    let id = ResourceId::local(4);
    let mut slot = crate::attach::pane_state::PaneSlot::new().expect("slot");
    slot.history_degraded = true;
    let mut panes = HashMap::from([(id.clone(), slot)]);
    assert_eq!(
        supervisory_badge(&panes, Some(&id), None).as_deref(),
        Some(" no-scrollback ")
    );
    panes.get_mut(&id).expect("slot").exited = Some(ExitMark {
        status: Some(1),
        signal: None,
    });
    assert_eq!(
        supervisory_badge(&panes, Some(&id), None).as_deref(),
        Some("[ exited 1 ]")
    );
}

/// The status bar names a down satellite, and the layout
/// leaf that owns the slot is not this function's to remove.
#[test]
fn a_down_satellite_pane_badges_its_host_on_the_status_bar() {
    let id = ResourceId::satellite("devbox", 7);
    let mut slot = crate::attach::pane_state::PaneSlot::new().expect("slot");
    slot.satellite_down = true;
    let panes = HashMap::from([(id.clone(), slot)]);
    assert_eq!(
        supervisory_badge(&panes, Some(&id), None).as_deref(),
        Some(" devbox down ")
    );
}

#[test]
fn attention_hint_formats_every_count() {
    assert_eq!(format_attention_hint(0), None);
    assert_eq!(format_attention_hint(1).as_deref(), Some(" ask "));
    assert_eq!(format_attention_hint(3).as_deref(), Some(" ask·3 "));
}

#[test]
fn window_infos_flags_zoom_only_on_the_active_window() {
    // The active window's `zoomed` reflects the zoom state;
    // a non-active window is never marked zoomed.
    let active = ResourceId::local(1);
    let mut workspace = ArrangementDoc::single(active.clone());
    workspace.add_group("2".to_owned(), ResourceId::local(2));
    workspace.select(0); // active window is index 0
    let panes: HashMap<ResourceId, PaneSlot> = HashMap::new();

    let infos = window_infos(
        &workspace,
        &panes,
        Some(&active),
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert!(infos[0].zoomed, "active window reflects the zoom state");
    assert!(!infos[1].zoomed, "a non-active window is never zoomed");

    // No zoom ⇒ no window is marked.
    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert!(!infos[0].zoomed && !infos[1].zoomed);
}

/// An auto-numbered window has not been named, so its tab says where it
/// is working instead of a second number beside the selector.
#[test]
fn an_auto_numbered_window_is_labelled_by_where_it_works() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let (_, _, mut panes) = published_test_state(&[(&id, 80, 24, b"")]);
    panes.get_mut(&id).expect("slot").cwd = Some("/src/phux/".to_owned());
    let infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    assert_eq!(infos[0].name, "phux");
}

/// A name the user gave is the label they chose; it outranks the agent
/// record and the program's title.
#[test]
fn a_name_the_user_gave_beats_the_agent_and_the_title() {
    let id = ResourceId::local(1);
    let mut workspace = ArrangementDoc::single(id.clone());
    workspace.groups[0].name = "deploy".to_owned();
    let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"\x1b]2;~/src\x07")]);
    let records = HashMap::from([(
        id,
        AgentRecord {
            name: "claude".to_owned(),
            ..AgentRecord::default()
        },
    )]);
    let infos = window_infos(&workspace, &panes, None, &records, &mut VcsIndex::default());
    assert_eq!(infos[0].name, "deploy");
}

/// The tab badge is the focused pane's agent badge, from the same
/// vocabulary the sidebar paints.
#[test]
fn badge_windows_marks_the_focused_agent() {
    let id = ResourceId::local(1);
    let workspace = ArrangementDoc::single(id.clone());
    let (_, _, panes) = published_test_state(&[(&id, 80, 24, b"")]);
    let mut infos = window_infos(
        &workspace,
        &panes,
        None,
        &HashMap::new(),
        &mut VcsIndex::default(),
    );
    let agents = vec![AgentEntry {
        session: None,
        session_id: None,
        resource: None,
        window: 0,
        window_name: "1".to_owned(),
        pane: Some(0),
        name: "claude".to_owned(),
        state: AgentMetaState::Working,
        attention: false,
        host: None,
        seen: true,
    }];
    let theme = crate::render::Theme::default();
    badge_windows(&mut infos, &workspace, &agents, &theme);
    let badge = infos[0].badge.as_ref().expect("badged");
    assert_eq!(badge.glyph, crate::render::chrome::AGENT_WORKING_GLYPH);
    assert_eq!(
        badge.style.fg.as_deref(),
        Some(crate::render::theme::color_to_string(theme.agent_working).as_str())
    );

    // An unread `done` is the diamond; once looked at it is the ring.
    let done = AgentEntry {
        state: AgentMetaState::Done,
        seen: false,
        ..agents[0].clone()
    };
    badge_windows(&mut infos, &workspace, std::slice::from_ref(&done), &theme);
    let badge = infos[0].badge.as_ref().expect("badged");
    assert_eq!(badge.glyph, crate::render::chrome::AGENT_DONE_GLYPH);
    let reviewed = AgentEntry { seen: true, ..done };
    badge_windows(&mut infos, &workspace, &[reviewed], &theme);
    let badge = infos[0].badge.as_ref().expect("badged");
    assert_eq!(badge.glyph, crate::render::chrome::AGENT_IDLE_GLYPH);
}
