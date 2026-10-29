//! The painter's state: projections in, change detection, click targets.

use std::io::{self, Write};

use phux_config::widget::WindowInfo;
use ratatui::buffer::Buffer;

use crate::layout::Rect;
use crate::render::Theme;
use crate::render::chrome::sidebar_sections::{PluginSection, PluginSectionSpec, PluginShape};
use phux_protocol::ids::ResourceId;

use super::agent_filter;
use super::agent_filter::{AgentFilter, AgentScope, HostFilter};
use super::compose::emit_changed;
use super::layout::{roster_segment, segment_starts};
use super::rows::{SessionRosterTarget, SidebarRule, SidebarTarget, SidebarTargets};
use super::{AgentEntry, DEFAULT_AGENTS_SHARE, SessionRosterEntry, SidebarCounts};
#[cfg(doc)]
use super::{SidebarHit, hit_test, row_model};

/// VT painter for the window sidebar.
#[derive(Debug)]
pub struct SidebarPainter {
    pub(super) windows: Vec<WindowInfo>,
    /// Every agent the projection supplied, before the filter.
    agents: Vec<AgentEntry>,
    /// What the Agents panel admits.
    pub(super) agent_filter: AgentFilter,
    /// The agents the filter admits: the rows the panel lists.
    pub(super) needs_you: Vec<AgentEntry>,
    pub(super) roster: Vec<SessionRosterEntry>,
    /// ADR-0148: the declared plugin sections, and their last projection.
    plugin_specs: Vec<PluginSectionSpec>,
    pub(super) plugin: Vec<PluginSection>,
    pub(super) theme: Theme,
    /// Last successfully emitted cells. Keep only one projection copy and
    /// compare rendered rows so hidden/truncated changes emit no bytes.
    last: Option<(Rect, Buffer)>,
    dirty: bool,
    rule: SidebarRule,
    /// `[sidebar] agents-share`: percent of the body the Agents area
    /// reserves while any agent needs a human; `0` drops it.
    agents_share: u8,
    /// Window index under a live row drag, painted as the insertion
    /// marker. `None` when no window drag is over this strip.
    pub(super) drop_at: Option<usize>,
    /// Strip rows where the pane grid's rules (the rail and every horizontal
    /// divider reaching this side) meet the separator rule, drawn as tees so
    /// the two read as one frame. Empty when no rule meets it.
    junctions: Vec<u16>,
}

/// Open a satellite agent beside the focused pane. `None` when `resource`
/// is local: that click stays a session switch.
#[must_use]
pub fn satellite_open_action(
    resource: &ResourceId,
) -> Option<phux_config::keybind::ResolvedAction> {
    resource.host()?;
    let mut args = std::collections::BTreeMap::new();
    args.insert(
        "direction".to_owned(),
        toml::Value::String("horizontal".to_owned()),
    );
    args.insert(
        "resource".to_owned(),
        toml::Value::String(phux_client::selector::format_terminal_id(resource)),
    );
    Some(phux_config::keybind::ResolvedAction {
        action: "split-pane".to_owned(),
        args,
    })
}

impl SidebarPainter {
    /// A painter styled by `theme`, initially showing no windows.
    #[must_use]
    pub const fn new(theme: Theme) -> Self {
        Self {
            windows: Vec::new(),
            agents: Vec::new(),
            agent_filter: AgentFilter {
                scope: AgentScope::All,
                host: HostFilter::Any,
            },
            needs_you: Vec::new(),
            roster: Vec::new(),
            plugin_specs: Vec::new(),
            plugin: Vec::new(),
            theme,
            last: None,
            dirty: true,
            rule: SidebarRule::Trailing,
            agents_share: DEFAULT_AGENTS_SHARE,
            drop_at: None,
            junctions: Vec::new(),
        }
    }

    /// The theme this strip paints with; other chrome resolves badges
    /// through it so every surface shares one palette.
    #[must_use]
    pub const fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Put the separator rule on the pane-facing side. Painters call this
    /// with the reservation's edge before each paint, so the click targets
    /// snapshotted from [`Self::click_targets`] match the frame on screen.
    pub fn set_rule(&mut self, rule: SidebarRule) {
        if self.rule != rule {
            self.rule = rule;
            self.dirty = true;
        }
    }

    /// Tee the separator rule into the pane grid at strip `rows` (empty
    /// clears them). Painters call this with the rail row and the rows of the
    /// horizontal dividers that run into the strip, beside [`Self::set_rule`].
    pub fn set_junctions(&mut self, rows: &[u16]) {
        if self.junctions != rows {
            self.junctions = rows.to_vec();
            self.dirty = true;
        }
    }

    /// Replace the window list; true when it changed (callers without another
    /// paint trigger gate a repaint on it).
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.dirty = true;
        true
    }

    /// Replace the agent list (every agent, in stable order); the panel
    /// lists the ones the [filter](Self::set_agent_filter) admits. Same
    /// change-report contract as [`Self::set_windows`].
    pub fn set_agents(&mut self, agents: Vec<AgentEntry>) -> bool {
        if self.agents == agents {
            return false;
        }
        self.agents = agents;
        self.refilter();
        true
    }

    /// The Agents panel's filter.
    #[must_use]
    pub const fn agent_filter(&self) -> &AgentFilter {
        &self.agent_filter
    }

    /// The satellite machines among the agents, for stepping the `@` chip.
    #[must_use]
    pub fn agent_hosts(&self) -> Vec<String> {
        agent_filter::agent_hosts(&self.agents)
    }

    /// Narrow the Agents panel to `filter`. Same change-report contract as
    /// [`Self::set_windows`].
    pub fn set_agent_filter(&mut self, filter: AgentFilter) -> bool {
        if self.agent_filter == filter {
            return false;
        }
        self.agent_filter = filter;
        self.refilter();
        true
    }

    /// Consuming form of [`Self::set_agent_filter`] for construction sites.
    #[must_use]
    pub fn with_agent_filter(mut self, filter: AgentFilter) -> Self {
        self.agent_filter = filter;
        self
    }

    /// Recompute the listed rows from every agent and the filter. Always
    /// repaints: the chip row's shape follows the machine set, which can
    /// move while the listed rows stay the same.
    fn refilter(&mut self) {
        self.needs_you = self
            .agents
            .iter()
            .filter(|entry| self.agent_filter.admits(entry))
            .cloned()
            .collect();
        self.dirty = true;
    }

    /// Replace the Sessions roster, including the current session. Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_roster(&mut self, roster: Vec<SessionRosterEntry>) -> bool {
        if self.roster == roster {
            return false;
        }
        self.roster = roster;
        self.dirty = true;
        true
    }

    /// Declare the plugin sections (ADR-0148) the strip lays out; their rows
    /// arrive through [`Self::set_plugin_sections`].
    #[must_use]
    pub fn with_plugin_specs(mut self, specs: Vec<PluginSectionSpec>) -> Self {
        self.plugin_specs = specs;
        self
    }

    /// The declared plugin sections, for the projection that fills them.
    #[must_use]
    pub fn plugin_specs(&self) -> &[PluginSectionSpec] {
        &self.plugin_specs
    }

    /// Set the `[sidebar] agents-share` percent the row allocator reads. Same
    /// change-report contract as [`Self::set_windows`]: true when it moved, so
    /// a caller without another paint trigger can repaint on it.
    pub const fn set_agents_share(&mut self, agents_share: u8) -> bool {
        if self.agents_share == agents_share {
            return false;
        }
        self.agents_share = agents_share;
        self.dirty = true;
        true
    }

    /// Consuming form of [`Self::set_agents_share`] for construction sites.
    #[must_use]
    pub const fn with_agents_share(mut self, agents_share: u8) -> Self {
        self.agents_share = agents_share;
        self
    }

    /// Replace the plugin sections' projected rows. Same change-report
    /// contract as [`Self::set_windows`].
    pub fn set_plugin_sections(&mut self, sections: Vec<PluginSection>) -> bool {
        if self.plugin == sections {
            return false;
        }
        self.plugin = sections;
        self.dirty = true;
        true
    }

    /// The counts [`row_model`] and [`hit_test`] derive the strip's shape
    /// from.
    #[must_use]
    pub fn counts(&self) -> SidebarCounts {
        SidebarCounts {
            needs_you: self.needs_you.len(),
            windows: self.windows.len(),
            roster: self.roster.len(),
            active_session: self.roster.iter().position(|s| s.active),
            host_starts: segment_starts(self.roster.iter().map(roster_segment)),
            project_starts: self.project_starts(),
            rule: self.rule,
            plugin: PluginShape::of(&self.plugin),
            agents_share: self.agents_share,
            agent_host_chip: self.agent_filter.host != HostFilter::Any
                || !agent_filter::agent_hosts(&self.agents).is_empty(),
        }
    }

    /// The click-resolution table for the current projections — what turns a
    /// [`SidebarHit`] index back into an action.
    #[must_use]
    pub fn click_targets(&self) -> SidebarTargets {
        SidebarTargets {
            counts: self.counts(),
            needs_you: self
                .needs_you
                .iter()
                .map(|e| {
                    e.session
                        .as_ref()
                        .map_or(SidebarTarget::Window(e.window), |name| {
                            SidebarTarget::Session {
                                name: name.clone(),
                                id: e.session_id,
                                window: e.pane.map(|_| e.window),
                                pane: e.pane,
                                resource: e.resource.clone(),
                            }
                        })
                })
                .collect(),
            roster: self
                .roster
                .iter()
                .map(|s| {
                    s.selectable.then(|| SessionRosterTarget {
                        name: s.name.clone(),
                        id: s.id,
                        host: s.route_host.clone(),
                        switch_host: s.switch_host.clone(),
                    })
                })
                .collect(),
            projects: self
                .roster
                .iter()
                .map(|s| s.group.as_ref().map(|group| group.key.clone()))
                .collect(),
            plugin: self
                .plugin
                .iter()
                .map(|section| {
                    section
                        .entries
                        .iter()
                        .map(|row| (row.window, row.pane))
                        .collect()
                })
                .collect(),
        }
    }

    /// Point (or clear) the live insertion marker at window `drop_at`.
    /// Same change-report contract as [`Self::set_windows`].
    pub fn set_drop_index(&mut self, drop_at: Option<usize>) -> bool {
        if self.drop_at == drop_at {
            return false;
        }
        self.drop_at = drop_at;
        self.dirty = true;
        true
    }

    /// Drop the paint cache so the next [`Self::paint`] re-emits even if its
    /// inputs are unchanged (e.g. after a full-frame clear).
    pub fn invalidate(&mut self) {
        self.last = None;
    }

    /// Paint the sidebar into `rect` (outer-viewport cells). No-op when the
    /// rect is empty or unchanged since the last paint.
    pub fn paint<W: Write>(&mut self, out: &mut W, rect: Rect) -> io::Result<()> {
        if rect.w == 0 || rect.h == 0 {
            return Ok(());
        }
        if !self.dirty && self.last.as_ref().is_some_and(|(r, _)| *r == rect) {
            return Ok(());
        }
        let buf = self.compose(rect, self.rule, &self.junctions);
        let previous = self
            .last
            .as_ref()
            .filter(|(r, _)| *r == rect)
            .map(|(_, b)| b);
        emit_changed(out, &buf, rect, previous)?;
        self.last = Some((rect, buf));
        self.dirty = false;
        Ok(())
    }

    /// Compose the strip into a `rect`-sized buffer at origin `(0, 0)`, the
    /// same compose the VT [`Self::paint`] path uses.
    #[must_use]
    pub fn compose_buffer(&self, rect: Rect, rule: SidebarRule, junctions: &[u16]) -> Buffer {
        self.compose(rect, rule, junctions)
    }
}
