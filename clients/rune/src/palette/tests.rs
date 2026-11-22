use super::*;

/// The essentials read in teaching order and end at the full list, so a
/// newcomer who runs out of rows is one Return from every chord.
#[test]
fn essentials_end_with_the_full_list() {
    let items = essential_items();
    assert_eq!(items.len(), ESSENTIALS.len() + 1);
    for (item, action) in items.iter().zip(ESSENTIALS) {
        assert_eq!(item.target, Target::Action(*action));
        assert!(!item.detail.is_empty(), "{} shows no chord", item.title);
    }
    let last = items.last().expect("a last row");
    assert_eq!(last.target, Target::Action(Shortcut::KeyboardShortcuts));
    assert_eq!(last.title, "All Keyboard Shortcuts");
}

#[test]
fn subsequence_matches_and_misses() {
    assert!(score("nt", "New terminal").is_some());
    assert!(score("xyz", "New terminal").is_none());
    assert_eq!(score("", "anything"), Some((0, vec![])));
}

#[test]
fn word_starts_and_runs_outrank_scattered_matches() {
    let (initials, _) = score("nt", "New terminal").expect("matches");
    let (scattered, _) = score("nt", "inventory").expect("matches");
    assert!(initials > scattered, "{initials} vs {scattered}");
}

#[test]
fn matched_positions_are_title_chars() {
    let (_, at) = score("bt", "Bigger text").expect("matches");
    assert_eq!(at, vec![0, 7]);
}

#[test]
fn ranking_puts_the_best_title_first_and_falls_back_to_details() {
    let hits = rank(items(None, &[], &[]), "new tab");
    assert_eq!(hits[0].item.target, Target::Action(Shortcut::NewTerminal));
}

#[test]
fn the_shortcut_list_fits_and_shows_only_bound_commands() {
    let list = shortcut_items();
    assert!(list.len() <= ROWS, "{}", list.len());
    assert!(list.iter().all(|item| !item.detail.is_empty()));
    let tab = list.iter().find(|i| i.title == "Close tab");
    assert!(tab.is_some());
}

#[test]
fn every_theme_is_a_row_and_its_name_finds_it() {
    let hits = rank(items(None, &[], &[]), "nord");
    assert_eq!(hits[0].item.target, Target::Theme(Some("nord")));
    let themes = items(None, &[], &[])
        .iter()
        .filter(|i| matches!(i.target, Target::Theme(_)))
        .count();
    assert_eq!(themes, crate::theme::all().count() + 1);
}

#[test]
fn a_typed_name_offers_a_new_session_unless_taken() {
    let item = new_session_item(None, " api ").expect("offered");
    assert_eq!(item.target, Target::NewSession("api".to_owned()));
    assert!(new_session_item(None, "  ").is_none());
    let item = new_worktree_item(" feat/api ").expect("offered");
    assert_eq!(item.target, Target::NewWorktree("feat/api".to_owned()));
    assert!(new_worktree_item("two words").is_none());
}

#[test]
fn paths_read_below_their_origin() {
    let row = path_item("/repo/src/main.rs", false, "/repo");
    assert_eq!(row.title, "src/main.rs");
    assert_eq!(path_item("/etc", true, "/repo").title, "/etc/");
    assert_eq!(path_item("/repo/a\nb", false, "/repo").title, "a\u{fffd}b");
}

#[test]
fn sessions_group_by_project_with_linked_worktrees_marked() {
    use phux_client_runtime::project_identity::SessionProject;
    let project = |linked| SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/w/phux".to_owned(),
        branch: None,
        linked_worktree: linked,
        host: Some("mini".to_owned()),
    };
    let session = |id, name: &str, project| SessionDescriptor {
        id,
        name: name.to_owned(),
        attached_client_count: 0,
        keep_empty: false,
        created_at_unix_secs: 0,
        key: None,
        touched: None,
        lifetime: None,
        revision: None,
        members: 1,
        arrangement: None,
        awaiting_organization: false,
        project,
        project_link: None,
    };
    let mut topology = crate::desk::topology(
        vec![
            session(1, "scratch", None),
            session(2, "feat", Some(project(true))),
            session(3, "phux", Some(project(false))),
        ],
        Vec::new(),
    );
    let rows: Vec<_> = session_items(Some(&topology), &[])
        .into_iter()
        .map(|item| (item.title, item.detail))
        .collect();
    assert_eq!(
        rows,
        [
            ("phux".to_owned(), "phux · 1 terminal".to_owned()),
            ("└ feat".to_owned(), "phux · 1 terminal".to_owned()),
            ("scratch".to_owned(), "1 terminal".to_owned()),
        ],
        "the group leads, its primary checkout first"
    );
    // The stored order places the rest; the group still leads.
    let alpha = SessionKey::new([4; 16]);
    topology.sessions.push(SessionDescriptor {
        key: alpha,
        touched: None,
        ..session(4, "alpha", None)
    });
    let titles: Vec<_> = session_items(Some(&topology), &[alpha.expect("nonzero")])
        .into_iter()
        .map(|item| item.title)
        .collect();
    assert_eq!(titles, ["phux", "└ feat", "alpha", "scratch"]);
}

#[test]
fn an_empty_query_keeps_list_order() {
    let items = items(None, &[], &[]);
    let hits = rank(items.clone(), "");
    assert_eq!(hits.len(), items.len());
    assert_eq!(hits[0].item, items[0]);
}

#[test]
fn terminals_sit_under_their_tab_and_a_search_keeps_the_heading() {
    let terminal = |n: u32, title: &str| Item {
        title: title.to_owned(),
        detail: format!("work · @{n}"),
        kind: "terminal",
        target: Target::Terminal {
            id: ResourceId::local(n),
            session: "work".to_owned(),
        },
    };
    let tab = |title: &str, panes: [u32; 2]| TabGroup {
        title: title.to_owned(),
        session: "work".to_owned(),
        panes: panes.map(ResourceId::local).to_vec(),
    };
    let items: Vec<_> = [(1, "vim"), (2, "zsh"), (3, "claude"), (4, "htop")]
        .map(|(n, title)| terminal(n, title))
        .into();
    let tabs = [tab("editor", [1, 2]), tab("agents", [3, 4])];
    let rows = |query| {
        let hits = rank_grouped(items.clone(), &tabs, query);
        hits.into_iter()
            .map(|hit| (hit.item.kind, hit.item.title))
            .collect::<Vec<_>>()
    };
    let row = |kind, title: &str| (kind, title.to_owned());
    assert_eq!(
        rows(""),
        [
            row("tab", "editor"),
            row("pane", "vim"),
            row("pane", "zsh"),
            row("tab", "agents"),
            row("pane", "claude"),
            row("pane", "htop"),
        ]
    );
    // A pane brings its heading; a tab's title brings all its panes.
    assert_eq!(rows("htop"), [row("tab", "agents"), row("pane", "htop")]);
    assert_eq!(
        rows("editor"),
        [row("tab", "editor"), row("pane", "vim"), row("pane", "zsh")]
    );
}
