use phux_protocol::input::key::ModSet;

use super::*;

const C: ClipboardSelection = ClipboardSelection::Clipboard;

fn pane(id: u32) -> ResourceId {
    ResourceId::local(id)
}

fn reads() -> SharedClipboardReads {
    ClipboardReads::shared(true)
}

fn press(key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

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
    shared.borrow_mut().awaiting = None;
    // Granted for the pane: the next read is answered without a prompt.
    assert_eq!(
        shared
            .borrow_mut()
            .on_read(&pane(1), C, true, ClipboardRead::Ask, now),
        ReadAction::Query
    );
    assert_eq!(prompt.message, "pane 1 wants to read your clipboard");
}
