use super::*;

const C: ClipboardSelection = ClipboardSelection::Clipboard;

fn pane(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn reads() -> SharedClipboardReads {
    ClipboardReads::shared(true)
}

use crate::test_util::press;

#[test]
fn deny_an_unfocused_pane_and_an_old_server_never_prompt() {
    let now = Instant::now();
    let shared = reads();
    let mut r = shared.borrow_mut();
    for policy in [ClipboardRead::Allow, ClipboardRead::Ask] {
        assert_eq!(r.on_read(&pane(1), C, false, policy, now), ReadAction::Drop);
    }
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Deny, now),
        ReadAction::Drop
    );
    r.set_window_focused(false);
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Allow, now),
        ReadAction::Drop,
        "a window in the background never reads"
    );
    let old = ClipboardReads::shared(false);
    assert_eq!(
        old.borrow_mut()
            .on_read(&pane(1), C, true, ClipboardRead::Allow, now),
        ReadAction::Drop
    );
}

#[test]
fn allow_queries_the_host_and_the_answer_goes_to_the_asking_pane_only() {
    let now = Instant::now();
    let shared = reads();
    let mut r = shared.borrow_mut();
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Allow, now),
        ReadAction::Query
    );
    assert_eq!(
        r.take_query(now).as_deref(),
        Some(&b"\x1b]52;c;?\x1b\\"[..])
    );
    // One read at a time: a second one while the host answers is dropped.
    assert_eq!(
        r.on_read(&pane(2), C, true, ClipboardRead::Allow, now),
        ReadAction::Drop
    );
    assert_eq!(
        r.take_reply(b"aGk=", Some(&pane(1)), now),
        Some((pane(1), C, "hi".to_owned()))
    );
    assert_eq!(r.take_reply(b"aGk=", Some(&pane(1)), now), None, "once");
}

#[test]
fn an_answer_after_focus_moved_or_too_late_is_dropped() {
    let now = Instant::now();
    let shared = reads();
    let mut r = shared.borrow_mut();
    r.on_read(&pane(1), C, true, ClipboardRead::Allow, now);
    r.take_query(now);
    assert_eq!(r.take_reply(b"aGk=", Some(&pane(2)), now), None);
    r.on_read(&pane(1), C, true, ClipboardRead::Allow, now);
    r.take_query(now);
    let late = now + REPLY_TIMEOUT;
    assert_eq!(r.take_reply(b"aGk=", Some(&pane(1)), late), None);
}

#[test]
fn prompts_are_rate_limited_per_pane() {
    let now = Instant::now();
    let shared = reads();
    let mut r = shared.borrow_mut();
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Ask, now),
        ReadAction::Ask
    );
    r.decide(&pane(1), C, Choice::Deny);
    assert_eq!(r.take_query(now), None, "denied: nothing asked");
    let soon = now + Duration::from_secs(1);
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Ask, soon),
        ReadAction::Drop
    );
    assert_eq!(
        r.on_read(&pane(2), C, true, ClipboardRead::Ask, soon),
        ReadAction::Ask,
        "the limit is per pane"
    );
    r.decide(&pane(2), C, Choice::Deny);
    let later = now + PROMPT_COOLDOWN;
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Ask, later),
        ReadAction::Ask
    );
}

#[test]
fn the_prompt_allows_once_or_for_the_pane() {
    let now = Instant::now();
    let shared = reads();
    shared
        .borrow_mut()
        .on_read(&pane(1), C, true, ClipboardRead::Ask, now);
    let mut prompt =
        ClipboardReadOverlay::new(pane(1), C, "pane 1", Rc::clone(&shared), &Theme::default());
    // Typed straight after it appears, a key that allows does nothing.
    for key in [PhysicalKey::A, PhysicalKey::Enter, PhysicalKey::Y] {
        assert_eq!(prompt.handle_key(&press(key)), OverlayCommand::Stay);
    }
    prompt.shown -= PROMPT_ARMING;
    assert_eq!(
        prompt.handle_key(&press(PhysicalKey::A)),
        OverlayCommand::Dismiss
    );
    assert!(shared.borrow_mut().take_query(now).is_some());
    shared.borrow_mut().phase = ReadPhase::Idle;
    // Granted for the pane: the next read is answered without a prompt.
    assert_eq!(
        shared
            .borrow_mut()
            .on_read(&pane(1), C, true, ClipboardRead::Ask, now),
        ReadAction::Query
    );
    assert_eq!(prompt.message, "pane 1 wants to read your clipboard");
}

/// A prompt left unanswered past its timeout stops holding off reads, and
/// its late answer cannot replace the read that went ahead without it.
#[test]
fn a_late_answer_to_a_timed_out_prompt_leaves_the_next_read_alone() {
    let now = Instant::now();
    let shared = reads();
    let mut r = shared.borrow_mut();
    assert_eq!(
        r.on_read(&pane(1), C, true, ClipboardRead::Ask, now),
        ReadAction::Ask
    );
    let soon = now + Duration::from_secs(1);
    assert_eq!(
        r.on_read(&pane(2), C, true, ClipboardRead::Allow, soon),
        ReadAction::Drop,
        "an open prompt holds off other reads"
    );
    let later = now + ASK_TIMEOUT;
    assert_eq!(
        r.on_read(&pane(2), C, true, ClipboardRead::Allow, later),
        ReadAction::Query
    );
    r.decide(&pane(1), ClipboardSelection::Primary, Choice::AllowPane);
    assert_eq!(
        r.take_query(later).as_deref(),
        Some(&b"\x1b]52;c;?\x1b\\"[..])
    );
    assert_eq!(
        r.take_reply(b"aGk=", Some(&pane(2)), later),
        Some((pane(2), C, "hi".to_owned()))
    );
    assert!(r.granted.contains(&pane(1)), "a pane grant still sticks");
}

fn prompt_for(shared: &SharedClipboardReads, armed: bool) -> ClipboardReadOverlay {
    shared
        .borrow_mut()
        .on_read(&pane(1), C, true, ClipboardRead::Ask, Instant::now());
    let mut prompt =
        ClipboardReadOverlay::new(pane(1), C, "pane 1", Rc::clone(shared), &Theme::default());
    if armed {
        prompt.shown -= PROMPT_ARMING;
    }
    prompt
}

fn painted_rows(prompt: &ClipboardReadOverlay, area: Rect) -> Vec<String> {
    let mut buf = Buffer::empty(area);
    prompt.render(area, &mut buf);
    (0..area.height)
        .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// The dialog paints its title, the message, and all three buttons, all
/// inside its bounds.
#[test]
fn the_dialog_paints_its_message_and_buttons_within_bounds() {
    let prompt = prompt_for(&reads(), true);
    let area = Rect::new(0, 0, 80, 24);
    let rows = painted_rows(&prompt, area);
    let text = rows.join("\n");
    for needle in [
        "Clipboard read?",
        "pane 1 wants to read your clipboard",
        "Deny",
        "Allow pane",
        "Allow once",
    ] {
        assert!(text.contains(needle), "{needle} missing:\n{text}");
    }
    let bounds = prompt.bounds(area).expect("a modal");
    for (y, row) in (0u16..).zip(&rows) {
        for (x, ch) in (0u16..).zip(row.chars()) {
            let at = ratatui::layout::Position::new(x, y);
            assert!(
                ch == ' ' || bounds.contains(at),
                "{at:?} outside {bounds:?}"
            );
        }
    }
}

/// Enter presses the focused button: `Allow once` first, `Deny` after
/// Shift+Tab twice. Esc denies.
#[test]
fn enter_presses_the_focused_button_and_esc_denies() {
    let shared = reads();
    let mut prompt = prompt_for(&shared, true);
    assert_eq!(
        prompt.handle_key(&press(PhysicalKey::Enter)),
        OverlayCommand::Dismiss
    );
    assert!(
        shared.borrow_mut().take_query(Instant::now()).is_some(),
        "allowed once"
    );
    assert!(!shared.borrow().granted.contains(&pane(1)));

    let shared = reads();
    let mut prompt = prompt_for(&shared, true);
    let back = crate::test_util::key_event(
        PhysicalKey::Tab,
        phux_protocol::input::key::ModSet::SHIFT,
        None,
    );
    for _ in 0..2 {
        assert_eq!(prompt.handle_key(&back), OverlayCommand::Stay);
    }
    assert_eq!(
        prompt.handle_key(&press(PhysicalKey::Enter)),
        OverlayCommand::Dismiss
    );
    assert!(shared.borrow_mut().take_query(Instant::now()).is_none());

    let shared = reads();
    let mut prompt = prompt_for(&shared, false);
    assert_eq!(
        prompt.handle_key(&press(PhysicalKey::Escape)),
        OverlayCommand::Dismiss,
        "denying never waits"
    );
    assert!(shared.borrow_mut().take_query(Instant::now()).is_none());
}

/// Clicking `Allow pane` grants the pane, but not before the prompt arms;
/// `Deny` answers at once.
#[test]
fn clicking_a_button_answers_and_allowing_waits_to_arm() {
    use phux_protocol::input::key::ModSet;
    use phux_protocol::input::mouse::{MouseAction, MouseButton};
    let area = Rect::new(0, 0, 80, 24);
    let click = |prompt: &mut ClipboardReadOverlay, label: &str| {
        let (x, y) = (0u16..)
            .zip(painted_rows(prompt, area))
            .find_map(|(y, row)| row.find(label).map(|x| (x, y)))
            .expect("the button is painted");
        let at = |action| MouseEvent {
            action,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: f64::from(u16::try_from(x).expect("fits") + 1),
            y: f64::from(y),
        };
        assert_eq!(
            prompt.handle_mouse(&at(MouseAction::Press)),
            OverlayCommand::Stay
        );
        prompt.handle_mouse(&at(MouseAction::Release))
    };

    let shared = reads();
    let mut prompt = prompt_for(&shared, false);
    assert_eq!(click(&mut prompt, "Allow pane"), OverlayCommand::Stay);
    prompt.shown -= PROMPT_ARMING;
    assert_eq!(click(&mut prompt, "Allow pane"), OverlayCommand::Dismiss);
    assert!(shared.borrow().granted.contains(&pane(1)));

    let shared = reads();
    let mut prompt = prompt_for(&shared, false);
    assert_eq!(click(&mut prompt, "Deny"), OverlayCommand::Dismiss);
    assert!(shared.borrow_mut().take_query(Instant::now()).is_none());
}
