//! Plugin manifest `[[sidebar]]` sections in the TUI (ADR-0148).
//!
//! Each section's rows are this session's panes in window/leaf order, each
//! rendered from the section's `format`. Tokens resolve from state the client
//! already holds per pane (tab label, OSC title, cwd, last exit code, the
//! `phux.agent/v1` record), so a section adds no subscription, poll, or wire
//! surface. A pane contributes a row only when every token the format names
//! resolves for it: `{agent} {state}` lists agent panes, `{exit}` panes whose
//! last command reported a code.

use std::collections::HashMap;

use phux_client::agent_meta::AgentRecord;
use phux_config::plugin::PluginManifest;
use phux_config::widget::WindowInfo;
use phux_protocol::ids::ResourceId;

use crate::attach::pane_state::PaneSlot;
use crate::layout::Workspace;
use crate::render::chrome::sidebar_sections::{
    MAX_PLUGIN_SECTIONS, PluginSection, PluginSectionRow, PluginSectionSpec,
};

/// Flatten enabled manifests into section specs, in manifest then
/// declaration order, keeping at most [`MAX_PLUGIN_SECTIONS`].
#[must_use]
pub fn specs_from_manifests(manifests: &[PluginManifest]) -> Vec<PluginSectionSpec> {
    let mut specs = Vec::new();
    for manifest in manifests {
        for section in &manifest.sidebar {
            if specs.len() == MAX_PLUGIN_SECTIONS {
                tracing::warn!(
                    plugin = %manifest.id,
                    section = %section.id,
                    max = MAX_PLUGIN_SECTIONS,
                    "plugin sidebar section past the strip's limit; skipping",
                );
                continue;
            }
            specs.push(PluginSectionSpec {
                title: section.title.clone(),
                format: section.format.clone(),
                rows: section.rows,
            });
        }
    }
    specs
}

/// The per-pane state the tokens read.
struct PaneFacts<'a> {
    index: usize,
    window: &'a str,
    slot: Option<&'a PaneSlot>,
    agent: Option<&'a AgentRecord>,
    home: Option<&'a str>,
}

impl PaneFacts<'_> {
    /// One token's value for this pane; `None` when the pane lacks it (an
    /// unknown token cannot reach here: manifests are validated at load).
    fn token(&self, token: &str) -> Option<String> {
        let non_empty = |s: &str| {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_owned())
        };
        match token {
            "window" => non_empty(self.window),
            "index" => Some(self.index.to_string()),
            "title" => self.slot.and_then(|slot| non_empty(&slot.last_title)),
            "cwd" => self
                .slot
                .and_then(|slot| slot.cwd.as_deref())
                .and_then(non_empty)
                .map(|cwd| collapse_home(&cwd, self.home)),
            "exit" => self
                .slot
                .and_then(|slot| slot.last_exit)
                .map(|code| code.to_string()),
            "agent" => self.agent.and_then(|record| non_empty(&record.name)),
            "state" => self.agent.map(|record| record.state.as_str().to_owned()),
            _ => None,
        }
    }

    /// `format` with every token replaced, or `None` when any is missing.
    fn render(&self, format: &str) -> Option<String> {
        let mut out = String::with_capacity(format.len());
        let mut rest = format;
        while let Some(open) = rest.find('{') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('}') else {
                break;
            };
            out.push_str(&rest[..open]);
            out.push_str(&self.token(&after[..close])?);
            rest = &after[close + 1..];
        }
        out.push_str(rest);
        let text = out.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}

/// `$HOME` (at a component boundary) shown as `~`, as the `cwd` widget does.
fn collapse_home(cwd: &str, home: Option<&str>) -> String {
    match home.filter(|home| !home.is_empty()) {
        Some(home) if cwd == home => "~".to_owned(),
        Some(home) => cwd
            .strip_prefix(home)
            .and_then(|rest| rest.strip_prefix('/'))
            .map_or_else(|| cwd.to_owned(), |rest| format!("~/{rest}")),
        None => cwd.to_owned(),
    }
}

/// Project every spec over the session's panes. `windows` are the tab
/// labels (`window_infos`), index-aligned with `workspace.windows`.
#[must_use]
pub(in crate::attach) fn project(
    specs: &[PluginSectionSpec],
    workspace: &Workspace,
    windows: &[WindowInfo],
    panes: &HashMap<ResourceId, PaneSlot>,
    agents: &HashMap<ResourceId, AgentRecord>,
) -> Vec<PluginSection> {
    if specs.is_empty() {
        return Vec::new();
    }
    let home = std::env::var("HOME").ok();
    let mut facts = Vec::new();
    for (index, window) in workspace.windows.iter().enumerate() {
        let label = windows
            .get(index)
            .map_or(window.name.as_str(), |w| w.name.as_str());
        let leaves = window
            .state
            .tree
            .as_ref()
            .map(crate::layout::leaves)
            .unwrap_or_default();
        for (pane, id) in leaves.iter().enumerate() {
            facts.push((
                pane,
                PaneFacts {
                    index,
                    window: label,
                    slot: panes.get(id),
                    agent: agents.get(id),
                    home: home.as_deref(),
                },
            ));
        }
    }
    specs
        .iter()
        .map(|spec| PluginSection {
            title: spec.title.clone(),
            rows: spec.rows,
            entries: facts
                .iter()
                .filter_map(|(pane, facts)| {
                    Some(PluginSectionRow {
                        text: facts.render(&spec.format)?,
                        window: facts.index,
                        pane: *pane,
                    })
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::layout::{LayoutNode, SplitDir};
    use phux_client::agent_meta::AgentMetaState;

    fn rid(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    /// Window 0 holds pane 1; window 1 splits panes 2 | 3.
    fn workspace() -> Workspace {
        let mut ws = Workspace::single(rid(1));
        ws.add_window("build".to_owned(), rid(2));
        ws.windows[1].state.tree = Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(rid(2))),
            right: Box::new(LayoutNode::Leaf(rid(3))),
        });
        ws
    }

    fn slot(title: &str, cwd: Option<&str>, exit: Option<i32>) -> PaneSlot {
        let mut slot = PaneSlot::new_with_size(10, 2).expect("slot");
        slot.last_title = title.to_owned();
        slot.cwd = cwd.map(ToOwned::to_owned);
        slot.last_exit = exit;
        slot
    }

    fn spec(format: &str, rows: u8) -> PluginSectionSpec {
        PluginSectionSpec {
            title: "S".to_owned(),
            format: format.to_owned(),
            rows,
        }
    }

    fn texts(section: &PluginSection) -> Vec<(&str, usize, usize)> {
        section
            .entries
            .iter()
            .map(|row| (row.text.as_str(), row.window, row.pane))
            .collect()
    }

    #[test]
    fn a_pane_rows_only_when_every_token_resolves() {
        let ws = workspace();
        let windows: Vec<WindowInfo> = ["edit", "build"]
            .iter()
            .map(|name| WindowInfo {
                name: (*name).to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
                exited: None,
                badge: None,
            })
            .collect();
        let mut panes = HashMap::new();
        panes.insert(rid(1), slot("vim", Some("/repo"), None));
        panes.insert(rid(2), slot("", None, Some(2)));
        panes.insert(rid(3), slot("cargo", None, Some(0)));
        let mut agents = HashMap::new();
        agents.insert(
            rid(3),
            phux_client::agent_meta::parse_agent_record(
                br#"{"kind":"claude","name":"reviewer","state":"working"}"#,
            )
            .expect("record"),
        );
        let specs = [
            spec("{index}:{window} {title}", 3),
            spec("{window} exit {exit}", 3),
            spec("{agent} ({state})", 3),
            spec("all panes", 3),
        ];
        let sections = project(&specs, &ws, &windows, &panes, &agents);
        assert_eq!(
            texts(&sections[0]),
            [("0:edit vim", 0, 0), ("1:build cargo", 1, 1)]
        );
        assert_eq!(
            texts(&sections[1]),
            [("build exit 2", 1, 0), ("build exit 0", 1, 1)]
        );
        assert_eq!(texts(&sections[2]), [("reviewer (working)", 1, 1)]);
        assert_eq!(
            sections[3].entries.len(),
            3,
            "a constant row lists every pane"
        );
        assert_eq!(
            agents.get(&rid(3)).map(|r| r.state),
            Some(AgentMetaState::Working)
        );
    }

    #[test]
    fn cwd_collapses_home_at_a_component_boundary() {
        assert_eq!(collapse_home("/home/ab", Some("/home/ab")), "~");
        assert_eq!(collapse_home("/home/ab/src", Some("/home/ab")), "~/src");
        assert_eq!(collapse_home("/home/abc", Some("/home/ab")), "/home/abc");
        assert_eq!(collapse_home("/tmp", None), "/tmp");
    }

    #[test]
    fn specs_keep_declaration_order_and_cap_the_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux-plugin.toml");
        let mut body = String::from(
            "id = \"com.example.s\"\nname = \"S\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n",
        );
        for i in 0..6 {
            use std::fmt::Write as _;
            let _ = write!(
                body,
                "[[sidebar]]\nid = \"s{i}\"\ntitle = \"Section {i}\"\nformat = \"{{title}}\"\n"
            );
        }
        std::fs::write(&path, body).expect("write");
        let manifest = phux_config::plugin::load_plugin_manifest(&path).expect("load");
        let specs = specs_from_manifests(std::slice::from_ref(&manifest));
        assert_eq!(specs.len(), MAX_PLUGIN_SECTIONS);
        assert_eq!(specs[0].title, "Section 0");
        assert_eq!(
            specs[0].rows,
            phux_config::plugin::SIDEBAR_SECTION_DEFAULT_ROWS
        );
    }
}
