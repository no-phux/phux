//! Palette rows and the sections that list them: commands, settings, work,
//! and the Go To terminals, agents and tabs.

use gpui::{App, SharedString};
use phux_protocol::ResourceId;

use super::{Target, goto_facts, rank_sections};
use crate::app::desk::{self, Desk};
use crate::connection::AgentInfo;

/// One palette section: an optional heading and its rows.
#[derive(Clone)]
pub(super) struct Section {
    pub(super) label: Option<&'static str>,
    pub(super) rows: Vec<Row>,
}

#[derive(Clone)]
pub(super) struct Row {
    pub(super) title: SharedString,
    pub(super) detail: Option<SharedString>,
    /// Extra searchable text, not shown.
    pub(super) keywords: String,
    pub(super) target: Target,
    pub(super) command: Option<&'static str>,
    pub(super) category: &'static str,
    pub(super) chord: Option<String>,
    /// Agent badge, when this row is one. State chips read its predicates.
    pub(super) agent: Option<AgentInfo>,
    /// Passive metadata card. Absent for commands and for a row with nothing
    /// to show. Highlighting reads this; it does not open the terminal.
    pub(super) preview: Option<goto_facts::FactsCard>,
}

pub(super) fn simple_row(
    title: &str,
    detail: String,
    category: &'static str,
    target: Target,
) -> Row {
    Row {
        title: title.to_owned().into(),
        keywords: detail.clone(),
        detail: Some(detail.into()),
        target,
        command: None,
        category,
        chord: None,
        agent: None,
        preview: None,
    }
}

pub(super) fn settings_sections() -> Vec<Section> {
    vec![Section {
        label: Some("Settings"),
        rows: crate::app::settings_panel::Page::ALL
            .into_iter()
            .map(|page| {
                let mut row = simple_row(
                    page.title(),
                    page.description().to_owned(),
                    "Settings",
                    Target::Setting(page),
                );
                row.keywords = page.keywords().to_owned();
                row
            })
            .collect(),
    }]
}

/// Leads the detail of the Go To row this window shows, so the work, terminal
/// and tab in use stay legible among similar names and hosts.
pub(super) const CURRENT: &str = "Current · ";

pub(super) fn current_mark(current: bool) -> &'static str {
    if current { CURRENT } else { "" }
}

pub(super) fn work_rows(desk: &Desk) -> Vec<Row> {
    desk.work_catalog()
        .into_iter()
        .map(|work| {
            let destination = work.destination;
            let status = if work.online { "" } else { " · offline" };
            let intake = if work.intake { " · Intake" } else { "" };
            let mark = current_mark(desk.work_is_active(&destination));
            let detail = format!(
                "{mark}{} · {} terminals{intake}{status}",
                destination.host_label(),
                work.terminals
            );
            let mut row = simple_row(
                &destination.name.clone(),
                detail,
                "Work",
                Target::Work(destination),
            );
            if let Some((id, name)) = work.project {
                row.keywords.push_str(&format!(" {id} {name}"));
            }
            row
        })
        .collect()
}

/// Catalog visibility and runtime prerequisites, separate from grouping rows.
fn command_offered(
    command: &crate::settings::CommandSpec,
    desk: &Desk,
    organizing: bool,
    monitoring: bool,
) -> bool {
    if !command.is_listed() || matches!(command.id, "palette" | "goto") {
        return false;
    }
    if command.needs_organization() && !organizing {
        return false;
    }
    match command.id {
        "performance-monitor-stop" => monitoring,
        "agent-fleet" => desk.prefs.agent_fleet,
        _ => true,
    }
}

pub(super) fn command_sections(desk: &Desk, cx: &App) -> Vec<Section> {
    const ESSENTIALS: &[&str] = &["new", "native-agent", "split-right", "folder", "find"];
    let organizing = desk
        .link
        .as_ref()
        .is_some_and(|link| link.connection.organization());
    let monitoring = desk.desktop.read(cx).performance_is_open();
    let mut sections: Vec<Section> = Vec::new();
    for command in crate::settings::builtin_commands()
        .iter()
        .filter(|command| command_offered(command, desk, organizing, monitoring))
    {
        let mut keywords = String::from(command.id);
        for word in command.keywords {
            keywords.push(' ');
            keywords.push_str(word);
        }
        let row = Row {
            title: command.title.into(),
            detail: Some(command.description.into()),
            keywords,
            target: Target::Command,
            command: Some(command.id),
            category: command.group,
            chord: desk.shown_chord(command.id),
            agent: None,
            preview: None,
        };
        let label = if ESSENTIALS.contains(&command.id) {
            "Essentials"
        } else {
            command.group
        };
        if let Some(section) = sections
            .iter_mut()
            .find(|section| section.label == Some(label))
        {
            section.rows.push(row);
        } else {
            sections.push(Section {
                label: Some(label),
                rows: vec![row],
            });
        }
    }
    if let Some(essentials) = sections
        .iter_mut()
        .find(|section| section.label == Some("Essentials"))
    {
        essentials
            .rows
            .sort_by_key(|row| ESSENTIALS.iter().position(|id| Some(*id) == row.command));
    }
    sections
}

/// A small launch list on open; typing always searches the complete catalogue.
pub(super) fn command_results(desk: &Desk, cx: &App, query: &str) -> Vec<Section> {
    let mut sections = command_sections(desk, cx);
    if !query.trim().is_empty() {
        sections.extend(settings_sections());
        return rank_sections(sections, query);
    }
    if !desk.palette_view.all_commands {
        sections.retain(|section| section.label == Some("Essentials"));
    }
    for section in &mut sections {
        section.label = None;
    }
    vec![Section {
        label: None,
        rows: sections
            .into_iter()
            .flat_map(|section| section.rows)
            .collect(),
    }]
}

pub(super) fn goto_sections(desk: &Desk) -> Vec<Section> {
    let Some(link) = desk.link.as_ref() else {
        return vec![Section {
            label: Some("Work"),
            rows: work_rows(desk),
        }];
    };
    let mut agents = Vec::new();
    let mut terminals = Vec::new();
    let focused = desk.focused_terminal();
    for pane in link.connection.panes() {
        let mark = current_mark(focused.as_ref() == Some(&pane.terminal_id));
        let agent = link.connection.agents.get(&pane.terminal_id);
        let title = desk.pane_title(Some(&pane.terminal_id));
        let directory = desk::short_path(pane.cwd.clone());
        let detail = format!("{} · {directory}", pane.session_name);
        let card_title = agent
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| title.clone());
        let preview = goto_facts::facts_card(Some(&goto_facts::GotoSubject::Pane {
            title: card_title,
            agent: agent.cloned(),
            session: pane.session_name.clone(),
            directory,
            tab: tab_label(desk, &pane.terminal_id),
        }));
        let row = Row {
            title: agent
                .map(|info| format!("{} — {title}", info.name))
                .unwrap_or(title)
                .into(),
            keywords: agent
                .map(|info| format!("{detail} {}", info.word()))
                .unwrap_or_else(|| detail.clone()),
            detail: Some(
                (match agent.map(|info| info.word()) {
                    Some(word) if !word.is_empty() => format!("{mark}{word} · {detail}"),
                    _ => format!("{mark}{detail}"),
                })
                .into(),
            ),
            target: Target::Pane(pane.terminal_id.clone()),
            command: None,
            category: if agent.is_some() {
                "Agents"
            } else {
                "Terminals"
            },
            chord: None,
            agent: agent.cloned(),
            preview,
        };
        if agent.is_some() {
            agents.push(row)
        } else {
            terminals.push(row)
        }
    }
    // A one-pane tab is already listed as its terminal.
    let tabs = link
        .workspace
        .tabs
        .iter()
        .enumerate()
        .filter_map(|(index, tab)| {
            let placed = crate::workspace::layout::placements(&tab.root);
            if placed.len() < 2 {
                return None;
            }
            let mark = current_mark(tab.id == link.workspace.active_id);
            let detail = format!("{mark}Tab {} · {} panes", index + 1, placed.len());
            let title = desk::tab_title(desk, tab);
            let panes = placed
                .into_iter()
                .filter_map(|placement| tab_pane(desk, placement))
                .collect();
            Some(Row {
                title: title.clone().into(),
                keywords: detail.clone(),
                detail: Some(detail.into()),
                target: Target::Tab(tab.id.clone()),
                command: None,
                category: "Tabs",
                chord: None,
                agent: None,
                preview: goto_facts::facts_card(Some(&goto_facts::GotoSubject::Tab {
                    title,
                    panes,
                })),
            })
        })
        .collect::<Vec<_>>();
    [
        ("Work", work_rows(desk)),
        ("Agents", agents),
        ("Terminals", terminals),
        ("Tabs", tabs),
    ]
    .into_iter()
    .filter(|(_, rows)| !rows.is_empty())
    .map(|(label, rows)| Section {
        label: Some(label),
        rows,
    })
    .collect()
}

/// The tab that shows `terminal`, by the same title the tab row uses.
pub(super) fn tab_label(desk: &Desk, terminal: &ResourceId) -> Option<String> {
    let link = desk.link.as_ref()?;
    let tab = link.workspace.tabs.iter().find(|tab| {
        crate::workspace::layout::placements(&tab.root)
            .iter()
            .any(|placement| placement.terminal_id.as_ref() == Some(terminal))
    })?;
    Some(desk::tab_title(desk, tab))
}

/// One pane on a tab card: the agent name and its state word, or the terminal
/// title and its directory. Layout metadata only.
pub(super) fn tab_pane(
    desk: &Desk,
    placement: &crate::workspace::layout::Placement,
) -> Option<goto_facts::Fact> {
    let id = placement.terminal_id.as_ref()?;
    let link = desk.link.as_ref()?;
    let pane = link.connection.pane(id)?;
    let agent = link.connection.agents.get(id);
    let name = agent
        .map(|info| info.name.clone())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| desk.pane_title(Some(id)));
    let detail = agent
        .and_then(|info| {
            let word = info.word();
            (!word.is_empty()).then(|| word.to_owned())
        })
        .unwrap_or_else(|| desk::short_path(pane.cwd.clone()));
    Some(goto_facts::Fact::new(name, detail))
}
