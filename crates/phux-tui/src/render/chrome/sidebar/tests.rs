use super::*;

use phux_config::widget::WindowInfo;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use super::compose::hidden_counts;
use super::layout::{agents_budget, opens_segment};
use crate::layout::Rect;
use crate::render::Theme;
use crate::render::chrome::sidebar_sections::PluginSection;
use crate::render::display_width;

mod agents;
mod attention;
mod bounds;
mod frame;
mod hit_test;
mod row_model;
mod windows;

const fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
    Rect { x, y, w, h }
}

fn win(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
        exited: None,
        badge: None,
    }
}

fn win_attention(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        attention: true,
        ..win(name, active)
    }
}

fn win_exited(name: &str, active: bool, status: &str) -> WindowInfo {
    WindowInfo {
        exited: Some(status.to_owned()),
        ..win(name, active)
    }
}

fn win_branch(name: &str, active: bool, branch: &str) -> WindowInfo {
    WindowInfo {
        branch: Some(branch.to_owned()),
        ..win(name, active)
    }
}

fn agent(window: usize, window_name: &str, name: &str, state: AgentMetaState) -> AgentEntry {
    AgentEntry {
        session: None,
        session_id: None,
        resource: None,
        window,
        window_name: window_name.to_owned(),
        pane: None,
        name: name.to_owned(),
        state,
        attention: false,
        host: None,
        seen: false,
    }
}

fn roster(name: &str, blocked: usize, working: usize, done_unvisited: usize) -> SessionRosterEntry {
    SessionRosterEntry {
        name: name.to_owned(),
        host: "mini".to_owned(),
        selectable: true,
        blocked,
        working,
        done_unvisited,
        ..SessionRosterEntry::default()
    }
}

fn active_roster() -> SessionRosterEntry {
    SessionRosterEntry {
        active: true,
        ..roster("development", 0, 0, 0)
    }
}

/// Split an emitted strip into its per-row glyph runs (the text
/// between one row's `CUP` and the next).
fn rows_of(painted: &str) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = painted.chars().peekable();
    let mut started = false;
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
            }
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    if d == 'H' {
                        if started {
                            rows.push(std::mem::take(&mut cur));
                        }
                        started = true;
                    }
                    break;
                }
            }
            continue;
        }
        if started {
            cur.push(c);
        }
    }
    if started {
        rows.push(cur);
    }
    rows
}

fn paint_to_string(painter: &mut SidebarPainter, rect: Rect) -> String {
    let mut out = Vec::new();
    painter.paint(&mut out, rect).expect("paint");
    String::from_utf8(out).expect("utf8")
}

/// Strip CSI escape sequences so an assertion can read the plain glyphs —
/// a styled (active) row interleaves a per-cell SGR between every cell, so
/// its label is not a contiguous substring of the raw byte stream.
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI is `ESC [ params... final`; the final byte of the
            // sequences we emit (`H`, `m`) is an ASCII letter, while the
            // introducer `[`, digits, and `;` are not — consume through
            // the first letter.
            for d in chars.by_ref() {
                if d.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Row `y` of the composed buffer as plain text (separator column
/// excluded).
fn row_text(buf: &Buffer, rect: Rect, y: u16) -> String {
    (0..rect.w.saturating_sub(1))
        .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
        .collect()
}

fn strip_text(p: &SidebarPainter, rect: Rect) -> String {
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let mut out = String::new();
    for y in 0..rect.h {
        let mut row: String = (0..rect.w)
            .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        row.truncate(row.trim_end().len());
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// Calm population retains both fixed areas and expands the active session.
#[test]
fn sectioned_layout_snapshot_quiet() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win_branch("phux", true, "main")]);
    p.set_roster(vec![
        active_roster(),
        roster("feat-auth", 0, 1, 0),
        SessionRosterEntry {
            unknown: 2,
            satellite: true,
            host: "devbox".to_owned(),
            ..roster("prod-3", 0, 0, 0)
        },
    ]);
    let rect = r(0, 0, 26, 18);
    insta::assert_snapshot!(strip_text(&p, rect));
}

/// A full Agents area overflows without moving Sessions.
#[test]
fn sectioned_layout_snapshot_attention() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win_branch("phux", true, "main")]);
    let mut queue = vec![
        agent(0, "phux", "codex", AgentMetaState::Blocked),
        AgentEntry {
            session: Some("feat-auth".to_owned()),
            pane: Some(0),
            ..agent(1, "edit", "claude", AgentMetaState::Blocked)
        },
    ];
    for i in 0..7 {
        queue.push(AgentEntry {
            session: Some(format!("wave-{i}")),
            pane: Some(0),
            ..agent(0, "run", "claude", AgentMetaState::Working)
        });
    }
    p.set_agents(queue);
    p.set_roster(vec![active_roster(), roster("feat-auth", 1, 0, 0)]);
    let rect = r(0, 0, 26, 18);
    insta::assert_snapshot!(strip_text(&p, rect));
}

// ---------- phux-fce4 / phux-foz.9: row model + hit-test ----------

fn counts(needs_you: usize, windows: usize, roster: usize) -> SidebarCounts {
    SidebarCounts {
        needs_you,
        windows,
        roster,
        active_session: None,
        host_starts: every_segment(roster),
        project_starts: 0,
        rule: SidebarRule::Trailing,
        plugin: PluginShape::default(),
        agents_share: DEFAULT_AGENTS_SHARE,
        agent_host_chip: false,
    }
}

/// Every entry on its own machine: one header per session, the densest
/// shape the strip can take.
fn every_segment(roster: usize) -> u128 {
    (0..roster.min(128)).fold(0, |mask, j| mask | (1u128 << j))
}

fn assert_model_items(c: SidebarCounts, rows: &[SidebarRow]) {
    let mut agents = Vec::new();
    let mut sessions = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        match row {
            SidebarRow::NeedsYou(j) => {
                assert!(*j < c.needs_you);
                agents.push(*j);
            }
            SidebarRow::RosterEntry(j) => {
                assert!(*j < c.roster);
                // A segment's header sits directly above its first
                // session, and only there.
                let header = y.checked_sub(1).and_then(|above| rows.get(above));
                assert_eq!(
                    header == Some(&SidebarRow::RosterHost(*j)),
                    opens_segment(c, *j),
                    "entry {j} at y={y}"
                );
                sessions.push(*j);
            }
            SidebarRow::RosterHost(j) => {
                assert!(opens_segment(c, *j));
                assert_eq!(rows.get(y + 1), Some(&SidebarRow::RosterEntry(*j)));
            }
            SidebarRow::WindowName(i) => assert!(*i < c.windows),
            _ => {}
        }
    }
    assert_eq!(agents, (0..agents.len()).collect::<Vec<_>>());
    assert_eq!(sessions, (0..sessions.len()).collect::<Vec<_>>());
}

fn plugin_section(title: &str, rows: u8, texts: &[&str]) -> PluginSection {
    PluginSection {
        title: title.to_owned(),
        rows,
        entries: texts
            .iter()
            .enumerate()
            .map(
                |(i, text)| crate::render::chrome::sidebar_sections::PluginSectionRow {
                    text: (*text).to_owned(),
                    window: i,
                    pane: 0,
                },
            )
            .collect(),
    }
}
