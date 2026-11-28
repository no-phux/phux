//! The inventory as the Sessions page organises it: what needs you, the
//! active sessions as headed cards, and the quiet ones folded under a band.
use rune::{El, col};

use super::section::{
    self, Tally, chevron, heading_body, heading_row, heading_title, inbox_heading, session_section,
};
use super::{Desk, Look, control, empty_state, grid, other_hosts, resource};
use crate::addressable::Addressable;

/// Every query word may match a different inventory field; never screen text.
pub(super) fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

fn resource_matches(desk: &Desk, resource: &Addressable, query: &str) -> bool {
    let pane = desk
        .connection
        .topology
        .as_ref()
        .and_then(|t| t.pane(&resource.id));
    let cwd = pane
        .and_then(|pane| pane.cwd.as_deref())
        .unwrap_or_default();
    let agent = desk.activities.agent_name(&resource.id).unwrap_or_default();
    matches(
        query,
        &[
            &resource.session,
            &resource.label,
            cwd,
            agent,
            desk.machines.label(),
        ],
    )
}

/// The inventory as a phone organises it: what needs you first, then each
/// session that is doing something or holds you, as a header over a card of
/// its panes, then the quiet ones folded under their names. A search lists
/// every match in place, unfolded.
pub(super) fn session_list(desk: &Desk, p: &Look) -> El {
    let phone = desk.phone.as_ref().expect("phone");
    let query = &phone.query.text;
    let searching = !query.trim().is_empty();
    let all = desk.addressable();
    let current = desk.selected_session_name();
    let mut sections = Vec::new();
    let mut count = 0;
    let mut quiet = Vec::new();
    for session in desk.ordered_sessions() {
        let members: Vec<_> = all.iter().filter(|r| r.session_id == session.id).collect();
        let filtered: Vec<_> = members
            .iter()
            .copied()
            .filter(|r| resource_matches(desk, r, query))
            .collect();
        if !matches(query, &[&session.name, desk.machines.label()]) && filtered.is_empty() {
            continue;
        }
        count += 1;
        let here = current.as_deref() == Some(&session.name);
        if !searching && !here && Tally::of(desk, &members).quiet() {
            quiet.push((session, members, filtered));
            continue;
        }
        let fold = (!searching).then(|| phone.folded.contains(&session.id));
        sections.push(session_section(desk, session, &members, &filtered, fold, p));
    }
    let mut list = col()
        .w_full()
        .gap(grid::block_gap(p))
        .children((!searching).then(|| needs_you(desk, &all, p)).flatten())
        .children(sections);
    if !quiet.is_empty() {
        list = list.child(quiet_band(desk, &quiet, phone.quiet_shown, p));
    }
    // Hub satellite resources aren't members of a local session, but still
    // belong in the navigator with their host-qualified resource addresses.
    let satellites: Vec<_> = all
        .iter()
        .filter(|r| r.id.host().is_some() && resource_matches(desk, r, query))
        .collect();
    if !satellites.is_empty() {
        count += satellites.len();
        list = list.child(section::rows(
            desk,
            &satellites,
            resource::Place::Elsewhere,
            p,
        ));
    }
    if count == 0 {
        list = list.child(empty_state(desk, query, p));
    }
    other_hosts(list, desk, query, p)
}

/// Every pane asking for you, across sessions, ahead of the map: the
/// question a person opens the app with. Their sessions still list them.
fn needs_you(desk: &Desk, all: &[Addressable], p: &Look) -> Option<El> {
    let waiting: Vec<_> = all
        .iter()
        .filter(|r| r.id.host().is_none() && desk.activities.mark(&r.id).needs_you())
        .collect();
    if waiting.is_empty() {
        return None;
    }
    let label = format!("Needs you, {}", waiting.len());
    Some(
        col()
            .key("phone-needs-you")
            .w_full()
            .gap(p.space.s1)
            .child(inbox_heading(
                "phone-needs-you-heading",
                "Needs you",
                waiting.len(),
                label,
                p,
            ))
            .child(section::rows(desk, &waiting, resource::Place::NeedsYou, p)),
    )
}

type Quiet<'a> = (
    &'a phux_client_runtime::control::SessionDescriptor,
    Vec<&'a Addressable>,
    Vec<&'a Addressable>,
);

/// Sessions with nothing moving and nothing asking: one band that names
/// them while folded and lists them, each under its header, when shown.
fn quiet_band(desk: &Desk, quiet: &[Quiet<'_>], shown: bool, p: &Look) -> El {
    let s = &p.space;
    let names = quiet
        .iter()
        .map(|(session, ..)| session.name.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    let label = format!(
        "Quiet, {}, {}",
        match quiet.len() {
            1 => "1 session".to_owned(),
            n => format!("{n} sessions"),
        },
        if shown { "shown" } else { "folded" }
    );
    let summary = (!shown).then_some(names);
    let header = heading_row(
        control("phone-quiet", &label, p, |d, _| d.toggle_phone_quiet()),
        p,
    )
    .expanded(shown)
    .child(heading_body(
        heading_title("Quiet".to_owned(), p),
        summary,
        p,
    ))
    .child(chevron(!shown, p));
    let mut band = col()
        .key("phone-quiet-band")
        .w_full()
        .gap(s.s2)
        .child(header);
    if shown {
        let folded = &desk.phone.as_ref().expect("phone").folded;
        for (session, members, filtered) in quiet {
            band = band.child(session_section(
                desk,
                session,
                members,
                filtered,
                Some(folded.contains(&session.id)),
                p,
            ));
        }
    }
    band
}
