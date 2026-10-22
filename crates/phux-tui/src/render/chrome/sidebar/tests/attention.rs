//! The attention ladder and the agent state glyphs.

use super::*;

/// The attention ladder, rung by rung. The one that matters: an UNSEEN
/// `done` agent outranks a `working` one — "finished but you haven't
/// looked at it" is a request for a human; "still working" is not.
#[test]
fn attention_rank_puts_unreviewed_done_above_working() {
    use AgentMetaState as S;
    let blocked = attention_rank(S::Blocked, false, false);
    let done_unseen = attention_rank(S::Done, false, false);
    let working = attention_rank(S::Working, false, false);
    let done_seen = attention_rank(S::Done, false, true);
    let idle = attention_rank(S::Idle, false, true);
    let unknown = attention_rank(S::Unknown, false, true);

    assert!(blocked > done_unseen, "blocked outranks unreviewed done");
    assert!(done_unseen > working, "unreviewed done outranks working");
    assert!(working > done_seen, "working outranks a reviewed done");
    assert_eq!(done_seen, idle, "a reviewed done is as quiet as idle");
    assert!(idle > unknown, "an undeclared agent ranks last");

    // Visiting the pane is what demotes a finished agent — nothing else.
    assert!(attention_rank(S::Done, false, true) < attention_rank(S::Working, false, false));

    // An explicit attention flag (a declared high-attention record, or the
    // ADR-0035 asked flag) pins the row to the top rung whatever the state
    // says — an agent that asked for a human IS blocked on one.
    for state in [S::Idle, S::Working, S::Done, S::Unknown, S::Blocked] {
        assert_eq!(attention_rank(state, true, true), blocked, "{state:?}");
    }

    // `seen` is inert for every state but `done`: a blocked agent you
    // looked at is still blocked.
    for state in [S::Idle, S::Working, S::Blocked, S::Unknown] {
        assert_eq!(
            attention_rank(state, false, true),
            attention_rank(state, false, false),
            "{state:?}"
        );
    }
}

/// A roster row's dot and its agents' badges must agree about severity.
/// They are two renderings of ONE ladder, so `top_rank` returns the same
/// rungs `attention_rank` does — a session painted calm while one of its
/// agents displays a blocked badge would discredit the whole strip.
#[test]
fn roster_top_rank_follows_the_attention_ladder() {
    use AgentMetaState as S;

    assert_eq!(
        roster("a", 1, 3, 2).top_rank(),
        attention_rank(S::Blocked, false, false),
        "one blocked pane pins the session to the blocked rung"
    );
    assert_eq!(
        roster("a", 0, 3, 2).top_rank(),
        attention_rank(S::Done, false, false),
        "unreviewed done outranks working at the session level too"
    );
    assert_eq!(
        roster("a", 0, 3, 0).top_rank(),
        attention_rank(S::Working, false, false),
    );

    let settled = SessionRosterEntry {
        settled: 4,
        ..roster("a", 0, 0, 0)
    };
    assert_eq!(settled.top_rank(), attention_rank(S::Idle, false, true));

    // A satellite session cannot be inspected from here (spec/L3.md §5),
    // so it lands on the bottom rung — explicitly unknown, never a calm
    // zero that reads as "nothing to see".
    let sat = SessionRosterEntry {
        unknown: 3,
        satellite: true,
        ..roster("prod-3", 0, 0, 0)
    };
    assert_eq!(sat.top_rank(), attention_rank(S::Unknown, false, true));
    assert_eq!(sat.total(), 3, "unknown panes still count toward the total");
    assert_eq!(roster("a", 1, 3, 2).total(), 6);
}

/// The unreviewed-`done` row must be visually distinct from both a
/// `working` row and a reviewed-`done` row — the glyph is what the user
/// scans for. Under the `need` filter only the unreviewed one stays.
#[test]
fn unreviewed_done_gets_its_own_glyph() {
    let mut p = SidebarPainter::new(Theme::default());
    p.set_windows(vec![win("a", true)]);
    let rect = r(0, 0, 30, 12);

    // Agents tops the strip under its filter chips, so its only row is
    // index 2.
    let mut unseen = agent(0, "a", "claude", AgentMetaState::Done);
    unseen.seen = false;
    p.set_agents(vec![unseen.clone()]);
    let row = row_text(&p.compose_buffer(rect, SidebarRule::Trailing, &[]), rect, 2);
    assert!(row.contains('◆'), "unreviewed done: {row:?}");

    let seen = AgentEntry {
        seen: true,
        ..unseen
    };
    let text = |e: &AgentEntry| -> String {
        p.agent_line(e, 28)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    };
    assert!(text(&seen).contains('○'), "reviewed done relaxes");
    let working = agent(0, "a", "claude", AgentMetaState::Working);
    assert!(text(&working).contains('◐'), "working");

    p.set_agents(vec![seen, working]);
    assert_eq!(p.counts().needs_you, 2, "`all` lists both");
    p.set_agent_filter(AgentFilter {
        scope: AgentScope::Attention,
        host: HostFilter::Any,
    });
    assert_eq!(p.counts().needs_you, 0, "neither needs a human");
}

/// An agent row carries the shared glyph for its state: an unread
/// `done` is the diamond and `working` the half ring.
#[test]
fn agent_rows_use_the_shared_state_glyphs() {
    let p = SidebarPainter::new(Theme::default());
    let text = |e: &AgentEntry| -> String {
        p.agent_line(e, 28)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    };
    let done = agent(0, "a", "claude", AgentMetaState::Done);
    assert!(text(&done).contains('◆'), "done");
    let working = agent(0, "a", "claude", AgentMetaState::Working);
    assert!(text(&working).contains('◐'), "working");
}

/// `seen` is a real display input, so a flip must bust the paint cache —
/// otherwise visiting a finished pane would leave the "look at me" glyph
/// on screen.
#[test]
fn seen_flip_busts_the_paint_cache() {
    let mut p = SidebarPainter::new(Theme::default());
    let done = agent(0, "a", "claude", AgentMetaState::Done);
    assert!(p.set_agents(vec![done.clone()]));
    assert!(!p.set_agents(vec![done.clone()]));
    assert!(p.set_agents(vec![AgentEntry { seen: true, ..done }]));
}
