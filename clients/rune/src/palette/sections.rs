//! The palette's list in sections, the grammar the phone's Sessions page
//! reads in: what needs you first, then terminals, sessions, commands and
//! themes; while typing, the best match alone as the top hit, then the
//! rest grouped by kind in the order each group's best match ranks.
//!
//! Grouping only reorders whole runs of [`Hit`]s; the ranking inside a
//! group, and so what `rank` scored, is left as it was.

use super::{Hit, Item, Target};

pub const NEEDS_YOU: &str = "Needs you";
pub const TOP_HIT: &str = "Top hit";
pub const SUGGESTED: &str = "Suggested";
pub const TERMINALS: &str = "Terminals";
pub const WINDOWS: &str = "Windows";
pub const SESSIONS: &str = "Sessions";
pub const CREATE: &str = "Create";
pub const COMMANDS: &str = "Commands";
pub const THEMES: &str = "Themes";
pub const FILES: &str = "Files";

/// An item kind the palette puts first, ahead of its own section.
pub const SUGGESTED_KIND: &str = "suggested";

/// The section an item belongs to, by what it is.
pub fn section(item: &Item) -> &'static str {
    match (&item.target, item.kind) {
        (_, SUGGESTED_KIND) => SUGGESTED,
        (Target::NewSession(_) | Target::NewWorktree(_), _) => CREATE,
        (Target::Theme(_), _) => THEMES,
        (Target::Session(_), _) => SESSIONS,
        (Target::Path(_), _) => FILES,
        (_, "document") => WINDOWS,
        (_, "terminal" | "tab" | "pane") => TERMINALS,
        _ => COMMANDS,
    }
}

/// Whether the first of `hits` stands alone as the top hit: there is a
/// query, something else to group, and it is not a tab's heading, whose
/// terminals follow it.
fn leads(hits: &[Hit], typed: bool) -> bool {
    typed && hits.len() > 1 && hits[0].item.kind != "tab"
}

/// The section a hit is listed under: a terminal asking for you leads an
/// empty list in its own.
fn label(hit: &Hit, typed: bool, urgent: &impl Fn(&Item) -> bool) -> &'static str {
    if !typed && hit.item.kind == "terminal" && urgent(&hit.item) {
        NEEDS_YOU
    } else {
        section(&hit.item)
    }
}

/// `hits` with each section's rows together: Needs you first, the rest
/// in the order their first row came. With a query the best match stays
/// first, alone, as the top hit. `urgent` says whether a terminal needs
/// you.
pub fn arrange(hits: Vec<Hit>, query: &str, urgent: impl Fn(&Item) -> bool) -> Vec<Hit> {
    let typed = !query.trim().is_empty();
    let lead = usize::from(leads(&hits, typed));
    let mut order: Vec<&'static str> = vec![NEEDS_YOU];
    for hit in &hits[lead..] {
        let at = label(hit, typed, &urgent);
        if !order.contains(&at) {
            order.push(at);
        }
    }
    let rank = |hit: &Hit| {
        let at = label(hit, typed, &urgent);
        order.iter().position(|o| *o == at).unwrap_or(order.len())
    };
    let mut hits = hits;
    let rest = hits.split_off(lead);
    let mut rest: Vec<_> = rest.into_iter().map(|hit| (rank(&hit), hit)).collect();
    // Stable: each group keeps its rows in their ranked order.
    rest.sort_by_key(|(rank, _)| *rank);
    hits.extend(rest.into_iter().map(|(_, hit)| hit));
    hits
}

/// Where each section's heading goes in arranged `hits`: the row index it
/// sits above and its title. A list of one section has none.
pub fn headings(
    hits: &[Hit],
    query: &str,
    urgent: impl Fn(&Item) -> bool,
) -> Vec<(usize, &'static str)> {
    let typed = !query.trim().is_empty();
    let top = leads(hits, typed);
    let mut out: Vec<(usize, &'static str)> = Vec::new();
    for (index, hit) in hits.iter().enumerate() {
        let at = if index == 0 && top {
            TOP_HIT
        } else {
            label(hit, typed, &urgent)
        };
        if out.last().is_none_or(|(_, last)| *last != at) {
            out.push((index, at));
        }
    }
    if out.len() < 2 {
        out.clear();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Shortcut;
    use phux_protocol::ids::ResourceId;

    fn hit(title: &str, kind: &'static str, target: Target) -> Hit {
        Hit {
            item: Item {
                title: title.to_owned(),
                detail: String::new(),
                kind,
                target,
            },
            score: 0,
            matched: Vec::new(),
        }
    }

    fn terminal(title: &str, n: u32) -> Hit {
        let target = Target::Terminal {
            id: ResourceId::local(n),
            session: "phux".to_owned(),
        };
        hit(title, "terminal", target)
    }

    fn command(title: &str) -> Hit {
        hit(title, "action", Target::Action(Shortcut::NewTerminal))
    }

    fn titles(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|h| h.item.title.as_str()).collect()
    }

    fn needs(title: &'static str) -> impl Fn(&Item) -> bool {
        move |item: &Item| item.title == title
    }

    #[test]
    fn an_empty_list_leads_with_what_needs_you_and_keeps_its_order() {
        let hits = vec![
            terminal("a", 1),
            terminal("b", 2),
            hit("phux", "session", Target::Session("phux".into())),
            command("New tab"),
            hit("Nord", "theme", Target::Theme(Some("nord"))),
        ];
        let arranged = arrange(hits, "", needs("b"));
        assert_eq!(titles(&arranged), ["b", "a", "phux", "New tab", "Nord"]);
        let heads = headings(&arranged, "", needs("b"));
        assert_eq!(
            heads,
            [
                (0, NEEDS_YOU),
                (1, TERMINALS),
                (2, SESSIONS),
                (3, COMMANDS),
                (4, THEMES)
            ]
        );
    }

    #[test]
    fn a_query_keeps_its_top_hit_then_groups_by_best_rank() {
        let hits = vec![
            command("Rename tab"),
            terminal("restyle", 1),
            command("Restore"),
            terminal("review", 2),
            hit("Retro", "theme", Target::Theme(Some("retro"))),
            command("Retry"),
        ];
        let arranged = arrange(hits, "re", needs("review"));
        assert_eq!(
            titles(&arranged),
            [
                "Rename tab",
                "restyle",
                "review",
                "Restore",
                "Retry",
                "Retro"
            ]
        );
        let heads = headings(&arranged, "re", needs("review"));
        assert_eq!(
            heads,
            [(0, TOP_HIT), (1, TERMINALS), (3, COMMANDS), (5, THEMES)]
        );
    }

    #[test]
    fn one_section_or_one_row_has_no_headings() {
        let only = vec![command("a"), command("b")];
        assert!(headings(&only, "", |_| false).is_empty());
        let one = vec![command("a")];
        assert!(headings(&arrange(one, "a", |_| false), "a", |_| false).is_empty());
    }

    #[test]
    fn a_tab_heading_keeps_its_terminals_and_is_no_top_hit() {
        let tab = |title: &str, kind| {
            let mut h = terminal(title, 1);
            h.item.kind = kind;
            h
        };
        let hits = vec![tab("work", "tab"), tab("one", "pane"), command("x")];
        let arranged = arrange(hits, "w", |_| false);
        assert_eq!(titles(&arranged), ["work", "one", "x"]);
        let heads = headings(&arranged, "w", |_| false);
        assert_eq!(heads, [(0, TERMINALS), (2, COMMANDS)]);
    }

    #[test]
    fn suggestions_and_creations_have_their_own_sections() {
        let suggested = hit("Paste", SUGGESTED_KIND, Target::Action(Shortcut::Paste));
        assert_eq!(section(&suggested.item), SUGGESTED);
        let new = hit("New", "action", Target::NewSession("x".into()));
        assert_eq!(section(&new.item), CREATE);
    }
}
