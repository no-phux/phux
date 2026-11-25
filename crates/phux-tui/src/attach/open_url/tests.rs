use super::*;
use crate::test_util::{press, press_mods, rendered};

const LINK: &str = "https://id.example/auth?redirect_uri=http%3A%2F%2Flocalhost%3A8085%2F";

fn pane() -> ResourceId {
    ResourceId::local(7)
}

/// A prompt for `url` that has been on screen, untouched, since `now`.
fn prompt(url: &str, now: Instant) -> (OpenUrlOverlay, SharedOpenUrls) {
    let urls = SharedOpenUrls::default();
    let link = urls
        .borrow_mut()
        .on_request(&pane(), 3, url, now)
        .expect("asks");
    let overlay = OpenUrlOverlay::new(
        (pane(), 3, link),
        ("mini", "work", Some("mini")),
        Rc::clone(&urls),
        now,
        &Theme::default(),
    );
    rendered(&overlay, 80, 24);
    overlay.quiet.set(Some(now));
    (overlay, urls)
}

fn answers(urls: &SharedOpenUrls) -> Vec<Option<String>> {
    urls.borrow_mut()
        .take_answers()
        .into_iter()
        .map(|answer| {
            assert_eq!((answer.terminal, answer.id), (pane(), 3));
            answer.link.map(|link| link.as_str().to_owned())
        })
        .collect()
}

#[test]
fn it_names_the_machine_pane_site_and_the_whole_link() {
    let (overlay, _urls) = prompt(LINK, Instant::now());
    let text = rendered(&overlay, 80, 24);
    let flat: String = text.split(['│', '\n']).map(str::trim).collect();
    for needle in [
        "pane 7 in work on mini asks to open this link here:",
        "id.example",
        LINK,
        "phux forward -L 8085:localhost:8085 --remote mini",
        "o open · esc decline",
    ] {
        assert!(flat.contains(needle), "{needle} missing:\n{text}");
    }
}

#[test]
fn opening_takes_o_after_a_quiet_second_and_nothing_typed_gets_there() {
    let start = Instant::now();
    let soon = start + QUIET / 2;
    // Typing: however long it goes on, no key in it answers.
    let (mut overlay, urls) = prompt(LINK, start);
    let mut now = start;
    for _ in 0..20 {
        for key in [
            PhysicalKey::O,
            PhysicalKey::Enter,
            PhysicalKey::N,
            PhysicalKey::Tab,
            PhysicalKey::Y,
        ] {
            now += QUIET / 2;
            assert_eq!(overlay.key_at(&press(key), now), OverlayCommand::Stay);
        }
    }
    // A pause, then any key but a bare o: still nothing opens.
    for key in [
        press(PhysicalKey::Y),
        press(PhysicalKey::Tab),
        press(PhysicalKey::Space),
        press_mods(PhysicalKey::O, ModSet::CTRL),
    ] {
        now += QUIET;
        assert_eq!(overlay.key_at(&key, now), OverlayCommand::Stay);
    }
    // A paste counts as typing.
    overlay.handle_paste("o");
    assert!(urls.borrow().answers.is_empty());

    overlay.quiet.set(Some(now));
    now += QUIET;
    let locked = press_mods(PhysicalKey::O, ModSet::NUM_LOCK);
    assert_eq!(overlay.key_at(&locked, now), OverlayCommand::Dismiss);
    drop(overlay);
    assert_eq!(answers(&urls), [Some(LINK.to_owned())], "answered once");

    // Enter declines; Esc declines at once.
    for (key, at) in [
        (PhysicalKey::Enter, start + QUIET),
        (PhysicalKey::Escape, soon),
    ] {
        let (mut overlay, urls) = prompt(LINK, start);
        assert_eq!(overlay.key_at(&press(key), at), OverlayCommand::Dismiss);
        assert_eq!(answers(&urls), [None]);
    }
}

#[test]
fn unanswered_it_expires_and_a_dropped_prompt_declines() {
    let start = Instant::now();
    let (mut overlay, urls) = prompt(LINK, start);
    let late = start + DEADLINE;
    overlay.key_at(&press(PhysicalKey::O), late);
    assert_eq!(answers(&urls), [None], "too late to open");

    let mut stack = crate::render::overlay::OverlayState::new();
    let (mut overlay, urls) = prompt(LINK, start);
    assert!(!overlay.expired());
    overlay.expires = start;
    stack.push(Box::new(overlay));
    assert!(stack.dismiss_expired());
    assert!(!stack.is_active());
    assert_eq!(answers(&urls), [None]);
}

#[test]
fn one_prompt_at_a_time_a_pause_after_a_decline_and_bad_links_are_declined_unasked() {
    let now = Instant::now();
    let ask = |urls: &SharedOpenUrls, url: &str, at| {
        urls.borrow_mut().on_request(&pane(), 3, url, at).is_some()
    };
    for bad in ["file:///etc/passwd", "https://a@evil.example/", "x"] {
        let urls = SharedOpenUrls::default();
        assert!(!ask(&urls, bad, now));
        assert_eq!(answers(&urls), [None]);
    }
    let (mut overlay, urls) = prompt(LINK, now);
    assert!(!ask(&urls, LINK, now), "another is up");
    assert_eq!(answers(&urls), [None]);
    overlay.key_at(&press(PhysicalKey::Escape), now);
    assert_eq!(answers(&urls), [None]);
    assert!(!ask(&urls, LINK, now + COOLDOWN / 2));
    assert!(ask(&urls, LINK, now + COOLDOWN));
}

/// A link longer than the screen scrolls to its end, and a screen too small
/// to show the site cannot open it.
#[test]
fn a_long_link_scrolls_and_an_unreadable_site_cannot_be_opened() {
    let start = Instant::now();
    let url = format!("https://id.example/{}END", "x".repeat(6990));
    let (mut overlay, _urls) = prompt(&url, start);
    let top = rendered(&overlay, 80, 24);
    assert!(top.contains("id.example") && !top.contains("END"), "{top}");
    for _ in 0..500 {
        overlay.key_at(&press(PhysicalKey::PageDown), start);
    }
    let end = rendered(&overlay, 80, 24);
    assert!(end.contains("END"), "{end}");

    let (mut overlay, urls) = prompt(LINK, start);
    rendered(&overlay, 20, 4);
    overlay.key_at(&press(PhysicalKey::O), start + QUIET);
    assert!(answers(&urls).is_empty());
}
