//! Palettes, the path picker, dialogs, and the find bar. Lists are the Kit
//! [`Command`] palette; text entry is Kit [`Input`]. Every modal is a
//! [`sheet`] on one backdrop: the same placement, a click outside or
//! Escape closes it, a click inside never does, and its control takes the
//! keyboard on open.

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, Context, Entity, EntityId, IntoElement, MouseButton,
    ParentElement, SharedString, Styled, Subscription, WeakEntity, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Icon, IconName, IndexPath, Sizable};
use phux_protocol::ResourceId;
use phux_protocol::ids::SessionKey;

use crate::connection::AgentInfo;
use crate::connection::path::{self as paths};

use super::color::from_hex;
use super::desk::{self, Desk, Modal};
use super::skin::{Look, Surface, Surfaced};
use super::ui::{dot, ghost_button, ghost_icon, muted};
use crate::settings::theme::Palette;

mod catalogue;
mod controls;
mod goto_facts;
mod goto_filter;
mod goto_frecency;
mod goto_search;
pub(crate) mod launcher;
mod panels;
mod path_picker;
mod surfaces;
mod windows;

use catalogue::*;
pub(crate) use controls::{find_input, prepare};
pub(crate) use goto_filter::GotoPalette;
pub(crate) use goto_frecency::Store as GotoFrecency;
pub(crate) use goto_search::GotoSearch;
use panels::*;
use path_picker::path_panel;
pub(crate) use path_picker::{PathItem, path_items};
#[cfg(test)]
use path_picker::{PathPresentation, path_presentation};
use surfaces::inset_radius;
pub(crate) use surfaces::{SHEET_GUTTER, card, corner, sheet, sheet_top};
pub use surfaces::{find_bar, layer};
pub(super) use windows::WINDOWS;
use windows::{raise_window, window_sections};

#[cfg(test)]
pub(super) mod palette_tests;
#[cfg(test)]
mod window_tests;

/// Transient disclosure state, reset whenever a palette opens.
#[derive(Default)]
pub(super) struct PaletteView {
    all_commands: bool,
    details: bool,
}

/// Text fields a modal or the find bar owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSlot {
    Find,
    Rename,
    SessionName,
    SessionDir,
    HostTarget,
    HostName,
}

/// What a palette row does when confirmed.
#[derive(Clone)]
enum Target {
    Command,
    Pane(ResourceId),
    Tab(String),
    Session(String),
    /// An open native window, and the tab to show there. Never opens one.
    Window {
        handle: AnyWindowHandle,
        desk: WeakEntity<Desk>,
        tab: Option<String>,
    },
    Work(super::fleet::Destination),
    Setting(super::settings_panel::Page),
    Move(SessionKey, String),
    Path(PathItem),
}

/// One palette section: an optional heading and its rows.
#[derive(Clone)]
struct Section {
    label: Option<&'static str>,
    rows: Vec<Row>,
}

#[derive(Clone)]
struct Row {
    title: SharedString,
    detail: Option<SharedString>,
    /// Extra searchable text, not shown.
    keywords: String,
    target: Target,
    command: Option<&'static str>,
    category: &'static str,
    chord: Option<String>,
    /// Agent badge, when this row is one. State chips read its predicates.
    agent: Option<AgentInfo>,
    /// Passive metadata card. Absent for commands and for a row with nothing
    /// to show. Highlighting reads this; it does not open the terminal.
    preview: Option<goto_facts::FactsCard>,
}

#[cfg(test)]
mod tests {
    use super::goto_filter::{GotoCursor, GotoFilter, GotoId, apply_filter, selection_path};
    use super::{
        AgentInfo, Row, Section, Target, filter_sections, goto_candidates,
        move_destination_sections, paste_question, rank_sections, section_ids,
    };

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
        use super::{PathPresentation, path_presentation};
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
            assert!(
                matches!(&sections[0].rows[0].target, Target::Move(key, _) if *key == work.key)
            );
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
}
