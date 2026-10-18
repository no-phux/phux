//! Palette rows, Go To filtering and ranking, Move destinations, and the
//! path picker's state, read without a window.

use super::dialogs::paste_question;
use super::goto_filter::{GotoCursor, GotoFilter, GotoId, apply_filter, selection_path};
use super::windows::move_destination_sections;
use super::{Row, Section, Target, filter_sections, goto_candidates, rank_sections, section_ids};
use crate::connection::AgentInfo;

#[test]
fn the_held_paste_question_names_the_lines_and_the_pane() {
    assert_eq!(paste_question("make", 3), "Paste 3 lines into make?");
    assert_eq!(paste_question("make", 1), "Paste a line break into make?");
}
use phux_protocol::ids::{Revision, SessionKey};
use phux_protocol::wire::frame::SessionRecord;

fn session(byte: u8, name: &str) -> SessionRecord {
    SessionRecord::new(SessionKey::new([byte; 16]).unwrap(), name, Revision::new(1))
}

#[test]
fn work_and_settings_rows_preserve_actual_indices_during_filter_realignment() {
    let work = crate::app::fleet::Destination {
        host: Some("mini".into()),
        authority: None,
        key: Some(SessionKey::new([3; 16]).unwrap()),
        name: "repo".into(),
    };
    let target = phux_protocol::ResourceId::local(7);
    let rows = vec![
        super::simple_row("repo", String::new(), "Work", Target::Work(work)),
        super::simple_row(
            "Appearance",
            String::new(),
            "Settings",
            Target::Setting(crate::app::settings_panel::Page::Appearance),
        ),
        super::simple_row(
            "agent",
            String::new(),
            "Agents",
            Target::Pane(target.clone()),
        ),
    ];
    let sections = vec![Section { label: None, rows }];
    let ids = section_ids(&sections);
    assert_eq!(ids[0].len(), 3);
    assert_eq!(
        selection_path(&ids, Some(&GotoId::Pane(target))),
        Some((0, 2))
    );
}

#[test]
fn path_state_reads_in_the_results_and_the_footer_keeps_paging() {
    use super::path_panel::{PathPresentation, path_presentation};
    let picker = crate::connection::path::begin_picker(
        crate::connection::path::PathTarget {
            terminal_id: "local:1".into(),
            placement_id: "place-1".into(),
            server_id: "a".into(),
            epoch: "1".into(),
        },
        "/repo",
    );
    let empty = |text: &str| PathPresentation {
        heading: None,
        empty: text.to_owned(),
        status: None,
    };
    let searching = crate::connection::path::start_query(picker.clone(), 1, "/repo", "");
    assert_eq!(
        path_presentation(&searching, false),
        empty("Searching host paths…")
    );
    let failed = crate::connection::path::PickerState {
        failure: Some("permission denied".into()),
        ..picker.clone()
    };
    assert_eq!(
        path_presentation(&failed, false),
        empty("permission denied")
    );
    assert_eq!(
        path_presentation(&failed, true),
        PathPresentation {
            heading: Some("permission denied".into()),
            empty: String::new(),
            status: None,
        },
        "a failed insertion heads the rows it left listed"
    );
    let warming = crate::connection::path::PickerState {
        message: "Host index is warming; search again shortly.".into(),
        ..picker.clone()
    };
    assert_eq!(
        path_presentation(&warming, false),
        empty("Host index is warming; search again shortly.")
    );
    assert_eq!(
        path_presentation(&warming, true).status.as_deref(),
        Some("Host index is warming; search again shortly.")
    );
    assert_eq!(path_presentation(&picker, false), empty("No paths."));
}

#[test]
fn move_intake_is_an_explicit_destination_not_a_named_session() {
    let intake = session(1, "legacy-home");
    let work = session(2, "Intake");
    let sections = move_destination_sections(&[&work, &intake], Some(intake.key), 2);
    assert_eq!(sections.len(), 2);
    assert!(
        matches!(&sections[0].rows[0].target, Target::Move(key, name) if *key == intake.key && name == "Intake")
    );
    assert!(matches!(&sections[1].rows[0].target, Target::Move(key, _) if *key == work.key));
}

#[test]
fn missing_or_ineligible_intake_never_fabricates_a_destination() {
    let work = session(2, "Work");
    for intake in [None, Some(session(1, "Intake").key)] {
        let sections = move_destination_sections(&[&work], intake, 1);
        assert_eq!(sections.len(), 1);
        assert!(matches!(&sections[0].rows[0].target, Target::Move(key, _) if *key == work.key));
    }
    assert!(move_destination_sections(&[], None, 1).is_empty());
}

fn ranked(items: &[&'static str], query: &str) -> Vec<String> {
    let rows = items
        .iter()
        .map(|title| Row {
            title: (*title).into(),
            detail: None,
            keywords: String::new(),
            target: Target::Command,
            command: None,
            category: "",
            chord: None,
            agent: None,
            preview: None,
        })
        .collect();
    rank_sections(vec![Section { label: None, rows }], query)
        .into_iter()
        .flat_map(|section| section.rows)
        .map(|row| row.title.to_string())
        .collect()
}

#[test]
fn subsequences_match_when_no_substring_does() {
    assert_eq!(
        ranked(&["Close Pane", "Split Right", "Settings"], "spr"),
        vec!["Split Right"]
    );
}

#[test]
fn word_starts_outrank_inner_hits_and_every_token_must_match() {
    assert_eq!(
        ranked(&["Unsplit", "Split Down", "Split Right"], "split"),
        vec!["Split Down", "Split Right", "Unsplit"]
    );
    assert_eq!(
        ranked(&["Split Down", "Split Right"], "split ri"),
        vec!["Split Right"]
    );
    assert!(ranked(&["Zoom"], "qq").is_empty());
}

fn agent(state: crate::connection::AgentState) -> AgentInfo {
    use crate::connection::AgentAttention;
    let attention = match state {
        crate::connection::AgentState::Blocked => AgentAttention::High,
        crate::connection::AgentState::Working => AgentAttention::Normal,
        crate::connection::AgentState::Idle => AgentAttention::None,
        crate::connection::AgentState::Done | crate::connection::AgentState::Unknown => {
            AgentAttention::Low
        }
    };
    AgentInfo {
        name: "agent".into(),
        kind: None,
        state,
        attention,
        changed_at_ms: 0,
    }
}

fn goto_row(title: &str, state: Option<crate::connection::AgentState>, id: u32) -> Row {
    Row {
        title: title.into(),
        detail: None,
        keywords: String::new(),
        target: Target::Pane(phux_protocol::ResourceId::local(id)),
        command: None,
        category: "Agents",
        chord: None,
        agent: state.map(agent),
        preview: None,
    }
}

#[test]
fn state_filter_then_rank_keeps_the_query_and_identity() {
    use crate::connection::AgentState;

    let sections = vec![Section {
        label: Some("Agents"),
        rows: vec![
            goto_row("Blocked", Some(AgentState::Blocked), 1),
            goto_row("Worker", Some(AgentState::Working), 2),
            goto_row("Other worker", Some(AgentState::Working), 5),
            goto_row("Idle", Some(AgentState::Idle), 3),
        ],
    }];
    let entries = goto_candidates(&sections);
    let worker = GotoId::Pane(phux_protocol::ResourceId::local(5));
    let mut cursor = GotoCursor {
        query: "other".into(),
        selected: Some(worker.clone()),
    };
    apply_filter(&mut cursor, &entries, GotoFilter::Working);
    assert_eq!(cursor.query, "other");
    assert_eq!(cursor.selected.as_ref(), Some(&worker));
    let ranked = rank_sections(
        filter_sections(sections, GotoFilter::Working),
        &cursor.query,
    );
    let ids = section_ids(&ranked);
    assert_eq!(ids, vec![vec![worker.clone()]]);
    assert_eq!(selection_path(&ids, cursor.selected.as_ref()), Some((0, 0)));

    let idle_row = || goto_row("Idle", Some(AgentState::Idle), 3);
    let idle_only = || {
        vec![Section {
            label: Some("Agents"),
            rows: vec![idle_row()],
        }]
    };
    let mut dropped = GotoCursor {
        query: "other".into(),
        selected: Some(worker),
    };
    apply_filter(
        &mut dropped,
        &goto_candidates(&idle_only()),
        GotoFilter::Idle,
    );
    assert_eq!(dropped.query, "other");
    assert!(dropped.selected.is_none());
    let hidden = rank_sections(
        filter_sections(idle_only(), GotoFilter::Idle),
        &dropped.query,
    );
    assert_eq!(
        selection_path(&section_ids(&hidden), dropped.selected.as_ref()),
        None,
        "a query that matches nothing leaves no row to highlight"
    );
    let shown = rank_sections(filter_sections(idle_only(), GotoFilter::Idle), "");
    assert_eq!(
        selection_path(&section_ids(&shown), dropped.selected.as_ref()),
        Some((0, 0))
    );
    assert!(matches!(
        &shown[0].rows[0].target,
        Target::Pane(id) if *id == phux_protocol::ResourceId::local(3)
    ));
}

#[test]
fn a_filter_keeps_the_highlighted_rows_facts_and_drops_a_hidden_card() {
    use super::goto_facts::{GotoSubject, facts_card};
    use crate::connection::AgentState;

    let card = facts_card(Some(&GotoSubject::Pane {
        title: "Worker".into(),
        agent: Some(agent(AgentState::Working)),
        session: "work".into(),
        directory: "~/repo".into(),
        tab: None,
    }))
    .expect("highlighted worker has a card");
    let mut worker = goto_row("Worker", Some(AgentState::Working), 2);
    worker.preview = Some(card.clone());
    let sections = vec![Section {
        label: Some("Agents"),
        rows: vec![worker, goto_row("Idle", Some(AgentState::Idle), 3)],
    }];
    let ranked = rank_sections(filter_sections(sections, GotoFilter::Working), "work");
    let shown = ranked[0].rows[0]
        .preview
        .as_ref()
        .expect("card follows the row");
    assert_eq!(shown, &card);
    assert_eq!(
        shown
            .facts
            .iter()
            .map(|fact| (fact.name.as_str(), fact.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("Status", "working"),
            ("Directory", "~/repo"),
            ("Session", "work"),
        ]
    );

    let hidden = rank_sections(
        filter_sections(
            vec![Section {
                label: Some("Agents"),
                rows: vec![goto_row("Idle", Some(AgentState::Idle), 3)],
            }],
            GotoFilter::Working,
        ),
        "",
    );
    assert!(hidden.is_empty(), "a hidden row takes its card with it");
    assert!(facts_card(None).is_none());
}
