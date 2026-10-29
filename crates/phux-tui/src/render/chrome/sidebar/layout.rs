//! The row model and the hit test that reads it.

use phux_config::widget::WindowInfo;

use crate::layout::Rect;
use crate::render::chrome::sidebar_sections::PluginShape;
use crate::render::display_width;

use super::agent_filter;
use super::rows::{SidebarHit, SidebarRow};
use super::{GUTTER, MIN_FOOTER_HEIGHT, SessionRosterEntry, SidebarCounts};

/// The row model: a fixed Agents band over Sessions (see the `agents_budget`
/// helper), plugin sections between them; name/host pairs never split. A
/// one-row strip keeps session navigation.
#[must_use]
pub fn row_model(counts: SidebarCounts, h: u16) -> Vec<SidebarRow> {
    let h = usize::from(h);
    let show_footer = h >= usize::from(MIN_FOOTER_HEIGHT);
    let body = h - usize::from(show_footer);
    let mut rows = Vec::with_capacity(h);

    if body == 1 {
        rows.push(SidebarRow::RosterOverflow);
    } else {
        // Plugin sections claim their fixed heights first, and only while
        // Agents and Sessions keep their minimum; population never decides.
        let sections = counts.plugin.fitting(body);
        let core = body - counts.plugin.height(sections);
        // ADR-0112: while the strip has anything to show, the Agents panel
        // keeps a fixed share of the body whatever its population, so gaining
        // or resolving an agent never moves Sessions. A strip with no content
        // at all paints no panels.
        let agents = if counts.needs_you > 0 || counts.roster > 0 {
            agents_budget(counts.agents_share, core)
        } else {
            0
        };
        push_agents(&mut rows, counts.needs_you, agents);
        rows.resize(agents, SidebarRow::Blank);
        push_plugin_sections(&mut rows, counts.plugin, sections);
        push_sessions(&mut rows, counts, core - agents);
        rows.resize(body, SidebarRow::Blank);
    }
    if show_footer {
        rows.push(SidebarRow::NewWindow);
    }
    rows
}

/// The Agents panel's height: `share` percent of the body rows left after the
/// plugin bands, rounded to nearest, kept to `1..core` so both panels keep at
/// least a header. Depends only on the viewport, the plugin bands, and the
/// share — never on the population (ADR-0112).
#[must_use]
pub(super) fn agents_budget(share: u8, core: usize) -> usize {
    if share == 0 || core < 2 {
        return 0;
    }
    let share = usize::from(share);
    let want = (core * share + 50) / 100;
    want.clamp(1, core - 1)
}

/// Keep overflow reachable even when only one content row fits. An empty
/// population paints a quiet placeholder, not a blank panel (ADR-0112).
fn push_agents(rows: &mut Vec<SidebarRow>, count: usize, budget: usize) {
    if budget == 0 {
        return;
    }
    rows.push(SidebarRow::NeedsYouHeader);
    let mut room = budget - 1;
    if room == 0 {
        return;
    }
    // The filter row only when a list row still fits under it.
    if room >= 2 {
        rows.push(SidebarRow::AgentFilter);
        room -= 1;
    }
    if count == 0 {
        rows.push(SidebarRow::NeedsYouEmpty);
        return;
    }
    let overflow = count > room;
    let shown = count.min(room - usize::from(overflow));
    rows.extend((0..shown).map(SidebarRow::NeedsYou));
    if overflow {
        rows.push(SidebarRow::NeedsYouOverflow);
    }
}

/// Append the first `n` plugin sections, each a header plus exactly its
/// reserved rows: entries, an empty placeholder, or entries with a `+N` last
/// row when the population overflows.
fn push_plugin_sections(rows: &mut Vec<SidebarRow>, shape: PluginShape, n: usize) {
    for (s, (reserved, count)) in shape.sections().take(n).enumerate() {
        let end = rows.len() + 1 + usize::from(reserved);
        rows.push(SidebarRow::PluginHeader(s));
        let room = usize::from(reserved);
        if count == 0 {
            rows.push(SidebarRow::PluginEmpty(s));
        } else if count > room {
            rows.extend((0..room - 1).map(|j| SidebarRow::PluginEntry(s, j)));
            rows.push(SidebarRow::PluginOverflow(s));
        } else {
            rows.extend((0..count).map(|j| SidebarRow::PluginEntry(s, j)));
        }
        rows.resize(end, SidebarRow::Blank);
    }
}

/// Append the fixed Sessions area with complete host pairs and nested windows.
fn push_sessions(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, budget: usize) {
    if budget == 0 || counts.roster == 0 {
        return;
    }
    let end = rows.len() + budget;
    rows.push(SidebarRow::SpacesHeader);
    let room = budget - 1;
    if room == 0 {
        return;
    }
    let want = session_row_count(counts);
    let reserve = want > room;
    let limit = end - usize::from(reserve);
    let start = rows.len();
    push_session_entries(rows, counts, limit);
    if rows.len() - start < want && rows.len() < end {
        rows.push(SidebarRow::RosterOverflow);
    }
}

fn session_row_count(counts: SidebarCounts) -> usize {
    let windows = counts
        .active_session
        .filter(|i| *i < counts.roster)
        .map_or(0, |_| counts.windows);
    let headers = (0..counts.roster.min(128))
        .filter(|j| opens_segment(counts, *j))
        .count();
    counts
        .roster
        .saturating_add(headers)
        .saturating_add(windows)
}

/// Whether roster entry `j` opens a machine segment (see
/// [`SidebarCounts::host_starts`]).
pub(super) const fn opens_segment(counts: SidebarCounts, j: usize) -> bool {
    j < 128 && counts.host_starts & (1u128 << j) != 0
}

/// The [`SidebarCounts::host_starts`] mask for `segments`, in roster order:
/// a segment is a project group (`true`, its key) or a host (`false`, its
/// label). One machine opens no segment: a lone host header says nothing.
pub(super) fn segment_starts<'a>(segments: impl Iterator<Item = (bool, &'a str)>) -> u128 {
    let mut mask = 0u128;
    let mut previous: Option<(bool, &str)> = None;
    for (j, segment) in segments.take(128).enumerate() {
        if previous != Some(segment) {
            mask |= 1u128 << j;
        }
        previous = Some(segment);
    }
    if mask.count_ones() < 2 { 0 } else { mask }
}

/// The roster segment entry `s` belongs to.
pub(super) fn roster_segment(s: &SessionRosterEntry) -> (bool, &str) {
    s.group
        .as_ref()
        .map_or((false, s.host.as_str()), |group| (true, group.key.as_str()))
}

/// Limit iteration by viewport capacity, even for extremely large counts.
/// A segment header and its first session are indivisible: a header never
/// paints without a session under it.
fn push_session_entries(rows: &mut Vec<SidebarRow>, counts: SidebarCounts, limit: usize) {
    for j in 0..counts.roster {
        let header = opens_segment(counts, j);
        if limit.saturating_sub(rows.len()) < 1 + usize::from(header) {
            break;
        }
        if header {
            rows.push(SidebarRow::RosterHost(j));
        }
        rows.push(SidebarRow::RosterEntry(j));
        if counts.active_session == Some(j) {
            let shown = counts.windows.min(limit - rows.len());
            rows.extend((0..shown).map(SidebarRow::WindowName));
        }
    }
}

/// Resolve an outer-viewport cell to a sidebar target; `None` off the strip,
/// on the rule, or on a blank row.
///
/// Headers open their management views; the
/// rule's bottom corner is the collapse chevron. `counts` must match the
/// painted frame.
#[must_use]
pub fn hit_test(rect: Rect, counts: SidebarCounts, x: u16, y: u16) -> Option<SidebarHit> {
    let local_x = x.checked_sub(rect.x)?;
    let local_y = y.checked_sub(rect.y)?;
    if local_x >= rect.w || local_y >= rect.h {
        return None;
    }
    let rule_x = counts.rule.column(rect.w);
    // The rule's bottom cell is the collapse chevron whenever the footer
    // renders (same condition the painter uses).
    if collapse_visible(rect) && local_x == rule_x && local_y == rect.h - 1 {
        return Some(SidebarHit::Collapse);
    }
    // The rest of the rule column is not a target.
    if local_x == rule_x {
        return None;
    }
    let row = *row_model(counts, rect.h).get(usize::from(local_y))?;
    if row == SidebarRow::AgentFilter {
        let col = local_x.checked_sub(counts.rule.content_x() + GUTTER)?;
        return agent_filter::filter_chips(text_width(rect.w), counts.agent_host_chip)
            .into_iter()
            .find(|(start, w, _)| col >= *start && col < start + w)
            .map(|(_, _, chip)| SidebarHit::AgentFilter(chip));
    }
    if let SidebarRow::RosterHost(j) = row
        && j < 128
        && counts.project_starts & (1u128 << j) != 0
    {
        return Some(SidebarHit::Project(j));
    }
    row_hit(row)
}

/// The text area's width inside the gutters and the separator rule.
pub(super) const fn text_width(strip_w: u16) -> u16 {
    strip_w.saturating_sub(1 + GUTTER * 2)
}

pub(super) const fn collapse_visible(rect: Rect) -> bool {
    rect.h >= MIN_FOOTER_HEIGHT && rect.w >= 2
}

const fn row_hit(row: SidebarRow) -> Option<SidebarHit> {
    match row {
        SidebarRow::WindowName(i) => Some(SidebarHit::Window(i)),
        SidebarRow::NeedsYou(j) => Some(SidebarHit::NeedsYou(j)),
        SidebarRow::RosterEntry(j) | SidebarRow::RosterHost(j) => Some(SidebarHit::Roster(j)),
        SidebarRow::NeedsYouHeader | SidebarRow::NeedsYouOverflow => Some(SidebarHit::Fleet),
        SidebarRow::SpacesHeader | SidebarRow::RosterOverflow => Some(SidebarHit::Sessions),
        SidebarRow::NewWindow => Some(SidebarHit::NewWindow),
        SidebarRow::PluginEntry(s, j) => Some(SidebarHit::Plugin(s, j)),
        SidebarRow::Blank
        | SidebarRow::AgentFilter
        | SidebarRow::NeedsYouEmpty
        | SidebarRow::PluginHeader(_)
        | SidebarRow::PluginEmpty(_)
        | SidebarRow::PluginOverflow(_) => None,
    }
}

/// Compact branch context is optional; never displace the window identity.
pub(super) fn fitting_branch(window: &WindowInfo, remaining: usize) -> Option<&str> {
    window
        .branch
        .as_deref()
        .filter(|branch| !branch.is_empty() && display_width(branch) < remaining)
}
