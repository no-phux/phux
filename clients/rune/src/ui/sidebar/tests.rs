use super::*;
use crate::theme::contrast;
use phux_config_core::host_color::PALETTE;
use phux_design::Tone;
use phux_design::tokens::IDENTITY;

fn tokens(dark: bool, theme: Option<&'static crate::theme::Theme>) -> std::rc::Rc<Tokens> {
    super::super::tokens_for(dark, theme)
}

#[test]
fn host_marks_read_in_every_theme() {
    let mut low = Vec::new();
    for theme in std::iter::once(None).chain(crate::theme::all().map(Some)) {
        for dark in [true, false] {
            let ink = tokens(dark, theme);
            let t: &Tokens = ink.as_ref();
            let sidebar = t.roles.layer.sidebar.paint();
            let ground = t.roles.layer.ground.paint();
            for (i, (id, _)) in PALETTE.iter().enumerate() {
                let host = host_color::swatch(i, sidebar);
                let name = theme.map_or("system", |t| t.id);
                // Eased for the sidebar; the focused card's rim sits on
                // the window ground, a shade away.
                let mark = contrast(host, sidebar);
                if mark < IDENTITY.host_bar || contrast(host, ground) < IDENTITY.host_bar - 0.1 {
                    low.push(format!("{id} on {name}/{dark}: {mark:.2}"));
                }
            }
        }
    }
    assert!(low.is_empty(), "{low:#?}");
}

#[test]
fn every_mark_draws_a_vector_mark_in_its_tone_with_its_word() {
    let ink = tokens(true, None);
    let t: &Tokens = ink.as_ref();
    for mark in [
        Mark::Blocked,
        Mark::Done,
        Mark::Failed,
        Mark::Bell,
        Mark::Working,
        Mark::Running,
        Mark::Background,
        Mark::Succeeded,
        Mark::Idle,
        Mark::None,
    ] {
        let f = face(mark, t);
        let state = state_of(mark);
        if mark == Mark::Bell {
            assert!(std::ptr::eq(f.icon, &icons::BELL));
            assert_eq!(f.ink, t.status.tone(Tone::Bell).ink);
        } else {
            assert_eq!(f.ink, t.status.state(state).tone.ink, "{mark:?}");
            assert_eq!(f.word, state.word(), "{mark:?}");
        }
        // Colour means state: a terminal's mark never borrows the accent.
        assert_ne!(f.ink, t.roles.accent.accent.paint(), "{mark:?}");
    }
    // `blocked` is retired from the UI; the canonical word is `needs you`.
    assert_eq!(face(Mark::Blocked, t).word, Some("needs you"));
    assert_eq!(face(Mark::Failed, t).word, Some("failed"));
    // The quiet class shares a mark; the words tell it apart.
    assert_ne!(face(Mark::Succeeded, t).word, face(Mark::Idle, t).word);
}

#[test]
fn the_histogram_counts_only_what_asks_for_you() {
    let mut counts = Counts::default();
    for mark in [
        Mark::Blocked,
        Mark::Bell,
        Mark::Failed,
        Mark::Done,
        Mark::Done,
        Mark::Working,
        Mark::Idle,
    ] {
        counts.add(mark);
    }
    assert_eq!(
        counts,
        Counts {
            needs_you: 2,
            failed: 1,
            done: 2
        }
    );
    assert_eq!(counts.waiting(), 5);
}

/// A window with one session of four terminals in different states.
fn listed(dark: bool) -> rune::Runtime<Desk> {
    let mut desk = crate::test_support::settings_window();
    desk.settings_open = false;
    let mut rt = rune::Runtime::new(
        desk,
        crate::ui::view,
        (1100.0, 720.0),
        rune::Env::new(dark, 1.0),
    );
    rt.frame(0.0);
    let session = phux_client_runtime::control::SessionDescriptor {
        id: 1,
        name: "work".to_owned(),
        attached_client_count: 1,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([1; 16]),
        touched: None,
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
        project: None,
        project_link: None,
    };
    let panes = (1..=4)
        .map(|id| phux_client_runtime::control::PaneDescriptor {
            terminal_id: ResourceId::local(id),
            session_id: 1,
            session_name: "work".to_owned(),
            cols: 80,
            rows: 24,
            title: Some(format!("pane {id}")),
            cwd: None,
            is_focused: id == 1,
            key: None,
            revision: None,
            name: Some(format!("pane {id}")),
            activation: None,
            exec: None,
            foreground: None,
        })
        .collect();
    let mut topology = crate::desk::topology(vec![session], panes);
    crate::test_support::server::terminal_resources(&mut topology);
    rt.update(move |d, _| {
        d.connection.topology = Some(topology);
        for (id, state) in [(1, "working"), (2, "blocked"), (3, "done"), (4, "idle")] {
            let event = crate::test_support::server::agent(id, "claude", state);
            d.activities.apply(&event, &[]);
        }
    });
    for t in [0.0, 1.0] {
        rt.frame(t);
    }
    rt
}

/// Whether an element keyed `key` takes focus. A terminal's sidebar row
/// and its card share the key; the row is the one that does.
pub(in crate::ui) fn focusable(rt: &rune::Runtime<Desk>, key: &str) -> bool {
    (rt.elements().iter()).any(|e| e.key.as_deref() == Some(key) && e.focusable)
}

#[test]
fn rows_are_one_focusable_source_list_with_their_words() {
    let rt = listed(true);
    let list = crate::ui::element(&rt, "sidebar-list");
    assert_eq!(list.role, "list");
    for id in 1..=4 {
        assert!(
            focusable(&rt, &format!("pane-{}", ResourceId::local(id))),
            "pane {id}"
        );
    }
    let header = crate::ui::element(&rt, "select-session-1");
    assert!(header.focusable, "the session header is focusable");
    // The word shows beside the mark, from the vocabulary; and the mark
    // is named by it where a screen reader meets it.
    let said = |words: &str| {
        rt.elements()
            .iter()
            .any(|e| e.text.as_deref() == Some(words) || e.label.as_deref() == Some(words))
    };
    for words in [
        "claude · needs you",
        "claude · working",
        "claude · done",
        "claude · idle",
    ] {
        assert!(said(words), "{words}");
    }
    assert!(
        said("needs you") && said("done") && said("idle"),
        "marks are named"
    );
}

#[test]
fn a_retheme_under_the_same_id_repaints_the_rows() {
    // Omarchy swaps palettes under one id: the memo keys on the tokens'
    // version, which follows the palette, not the id.
    let mut a = *crate::theme::find("catppuccin-mocha").expect("built in");
    let mut b = a;
    a.id = "omarchy";
    b.id = "omarchy";
    b.background = 0x10_20_30;
    let version = |theme: crate::theme::Theme| {
        Tokens::resolve(&super::super::tokens::Inputs {
            theme: Some(theme),
            ..super::super::tokens::Inputs::system(true)
        })
        .version
    };
    assert_ne!(version(a), version(b));
}
