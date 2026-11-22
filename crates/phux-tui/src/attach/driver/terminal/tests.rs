//! Terminal restoration, input mode, handoff, and panic guard tests.
use super::*;

static TERMINAL_RESET_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Serialize the tests that drive the process-global terminal mode flags.
fn reset_lock() -> std::sync::MutexGuard<'static, ()> {
    TERMINAL_RESET_TEST_LOCK
        .lock()
        .expect("terminal reset test lock")
}

/// Entry enables bracketed paste, focus reports, and (when configured)
/// mouse capture; the reset undoes them before leaving the alt screen.
#[test]
fn outer_terminal_modes_enable_and_disable_bytes() {
    let _guard = reset_lock();
    MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

    let mut entry = Vec::new();
    write_enter_alt_screen(&mut entry, true).unwrap();
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?2004h"),
        "entry must enable bracketed paste framing: {entry:?}"
    );
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?1004h"),
        "entry must enable focus reports: {entry:?}"
    );
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?1002h"),
        "entry must enable button-motion tracking: {entry:?}"
    );
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?1006h"),
        "entry must enable SGR coordinates: {entry:?}"
    );
    // Set by install on a real attach; needed for the full leave path.
    ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);
    // Reset emits the leave pair before the ?1049l alt-screen leave.
    let mut reset = Vec::new();
    write_terminal_reset(&mut reset).unwrap();
    let pos_2004l = reset
        .windows(8)
        .position(|w| w == b"\x1b[?2004l")
        .expect("reset must disable bracketed paste framing");
    assert!(
        reset.windows(8).any(|w| w == b"\x1b[?1004l"),
        "reset must disable focus reports: {reset:?}"
    );
    let pos_1006l = reset
        .windows(8)
        .position(|w| w == b"\x1b[?1006l")
        .expect("reset must disable SGR coordinates");
    let pos_1002l = reset
        .windows(8)
        .position(|w| w == b"\x1b[?1002l")
        .expect("reset must disable button-motion");
    let pos_1049l = reset
        .windows(8)
        .position(|w| w == b"\x1b[?1049l")
        .expect("reset must leave the alt screen");
    assert!(
        pos_2004l < pos_1049l && pos_1006l < pos_1049l && pos_1002l < pos_1049l,
        "outer-terminal mode resets must precede the alt-screen leave: {reset:?}"
    );
}

/// The kitty keyboard push lands on the alt screen (after `?1049h`) and
/// the shared reset pops it before `?1049l`; a reset with no alt screen
/// active pops nothing, so it never eats a stack entry phux did not push.
#[test]
fn host_kitty_keyboard_is_pushed_on_entry_and_popped_on_reset() {
    let _guard = reset_lock();
    let pos = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);

    let mut entry = Vec::new();
    write_enter_alt_screen(&mut entry, false).unwrap();
    let push = pos(&entry, b"\x1b[>5u").expect("entry pushes kitty flags");
    assert!(pos(&entry, b"\x1b[?1049h").expect("alt screen") < push);

    ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);
    let mut reset = Vec::new();
    write_terminal_reset(&mut reset).unwrap();
    let pop = pos(&reset, b"\x1b[<u").expect("reset pops kitty flags");
    assert!(pop < pos(&reset, b"\x1b[?1049l").expect("alt screen leave"));

    let mut again = Vec::new();
    write_terminal_reset(&mut again).unwrap();
    assert_eq!(pos(&again, b"\x1b[<u"), None, "idempotent reset: {again:?}");
}

#[test]
fn clean_detach_records_the_complete_reset_before_finalizing() {
    let _guard = reset_lock();
    HOVER_TRACKING_ACTIVE.store(true, Ordering::SeqCst);
    MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("detach.cast");
    let recorder = Rc::new(RefCell::new(
        SessionRecorder::create(&path, None, phux_record::cast::CastVersion::V2).expect("recorder"),
    ));
    let mut reset = Vec::new();
    write_terminal_reset_and_finalize(&mut reset, Some(&recorder));

    let cast = std::fs::read(&path).expect("read cast");
    let text = String::from_utf8(cast.clone()).expect("cast utf-8");
    assert!(
        text.lines()
            .next()
            .is_some_and(|line| line.contains("\"duration\":")),
        "clean detach must backfill duration"
    );
    let (_, events) = phux_record::cast::read_cast(cast.as_slice()).expect("parse cast");
    let recorded_reset: String = events
        .iter()
        .filter(|event| event.code == phux_record::cast::EventCode::Output)
        .map(|event| event.data.as_str())
        .collect();
    assert_eq!(
        recorded_reset.as_bytes(),
        reset,
        "the recorder must capture every reset byte before it is finalized"
    );
}

/// `mouse = false` skips mouse DECSET but keeps bracketed paste.
#[test]
fn mouse_capture_disabled_emits_no_decset() {
    let _guard = reset_lock();
    MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);

    let mut entry = Vec::new();
    write_enter_alt_screen(&mut entry, false).unwrap();
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?2004h"),
        "mouse=false must still enable bracketed paste: {entry:?}"
    );
    assert!(
        !entry.windows(8).any(|w| w == b"\x1b[?1002h"),
        "mouse=false must not enable tracking: {entry:?}"
    );
    assert!(
        entry.windows(8).any(|w| w == b"\x1b[?1049h"),
        "alt-screen enter still emitted: {entry:?}"
    );
    // With capture never set, reset emits no mouse-disable bytes.
    let mut reset = Vec::new();
    write_terminal_reset(&mut reset).unwrap();
    assert!(
        reset.windows(8).any(|w| w == b"\x1b[?2004l"),
        "reset must always disable bracketed paste: {reset:?}"
    );
    assert!(
        !reset.windows(8).any(|w| w == b"\x1b[?1002l"),
        "no capture ⇒ no mouse-disable on reset: {reset:?}"
    );
    assert!(
        !reset.windows(8).any(|w| w == b"\x1b[?1006l"),
        "no capture ⇒ no SGR mouse-disable on reset: {reset:?}"
    );
}

/// Leave pair when dropping, enter pair when restoring, nothing when
/// unchanged.
#[test]
fn sync_mouse_capture_emits_transitions_only() {
    let _guard = reset_lock();
    MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);

    // Already on ⇒ no bytes.
    let mut out = Vec::new();
    sync_mouse_capture(&mut out, true).unwrap();
    assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

    // On → off emits the reverse-order leave pair.
    sync_mouse_capture(&mut out, false).unwrap();
    assert_eq!(out, b"\x1b[?1006l\x1b[?1002l");

    // Off is now recorded ⇒ a second off is a no-op.
    out.clear();
    sync_mouse_capture(&mut out, false).unwrap();
    assert!(out.is_empty(), "idempotent off ⇒ no bytes: {out:?}");

    // Off → on emits the entry pair, and the reset path sees capture as
    // active again (the shared MOUSE_CAPTURE_ACTIVE flag).
    sync_mouse_capture(&mut out, true).unwrap();
    assert_eq!(out, b"\x1b[?1002h\x1b[?1006h");
    assert!(MOUSE_CAPTURE_ACTIVE.load(Ordering::SeqCst));

    MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
}

/// Hover is raised only on top of live capture and always unwound.
#[test]
fn sync_hover_tracking_rides_on_top_of_capture() {
    let _guard = reset_lock();
    MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
    HOVER_TRACKING_ACTIVE.store(false, Ordering::SeqCst);

    // No capture ⇒ the client has no business reporting motion.
    let mut out = Vec::new();
    sync_hover_tracking(&mut out, true).unwrap();
    assert!(out.is_empty(), "capture off ⇒ no hover bytes: {out:?}");
    assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

    // With capture live, opening a menu raises any-motion once.
    MOUSE_CAPTURE_ACTIVE.store(true, Ordering::SeqCst);
    sync_hover_tracking(&mut out, true).unwrap();
    assert_eq!(out, b"\x1b[?1003h");
    out.clear();
    sync_hover_tracking(&mut out, true).unwrap();
    assert!(out.is_empty(), "no transition ⇒ no bytes: {out:?}");

    // Closing it drops any-motion and re-asserts button-event tracking,
    // so divider drags survive the round trip.
    sync_hover_tracking(&mut out, false).unwrap();
    assert_eq!(out, b"\x1b[?1003l\x1b[?1002h");

    // Capture dropping (focus moved to an opted-out pane) while a menu
    // is open must not strand `?1003h` on the host terminal.
    out.clear();
    sync_hover_tracking(&mut out, true).unwrap();
    assert_eq!(out, b"\x1b[?1003h");
    out.clear();
    sync_mouse_capture(&mut out, false).unwrap();
    assert_eq!(out, b"\x1b[?1003l\x1b[?1006l\x1b[?1002l");
    assert!(!HOVER_TRACKING_ACTIVE.load(Ordering::SeqCst));

    MOUSE_CAPTURE_ACTIVE.store(false, Ordering::SeqCst);
}

/// Capture follows focus — wanted iff the global gate is on
/// AND the focused pane has not opted out.
#[test]
fn desired_mouse_capture_follows_focused_pane_optout() {
    let t1 = ResourceId::local(1);
    let t2 = ResourceId::local(2);
    let mut optout = std::collections::HashSet::new();
    optout.insert(t2.clone());

    // Global gate off wins unconditionally.
    assert!(!desired_mouse_capture(false, Some(&t1), &optout));
    assert!(!desired_mouse_capture(false, None, &optout));
    // Gate on: an opted-in focused pane (or none yet) keeps capture.
    assert!(desired_mouse_capture(true, Some(&t1), &optout));
    assert!(desired_mouse_capture(true, None, &optout));
    // Gate on but the focused pane opted out ⇒ capture drops.
    assert!(!desired_mouse_capture(true, Some(&t2), &optout));
}

/// `[chrome] title` fills each placeholder once and drops control bytes, so
/// a pane title cannot end the OSC early.
#[test]
fn outer_title_fills_placeholders_and_drops_controls() {
    assert_eq!(
        outer_title("{session}: {window}", "work", "vim"),
        "work: vim"
    );
    assert_eq!(outer_title("{session}", "{window}", "vim"), "{window}");
    assert_eq!(outer_title("{x} {window", "s", "w"), "{x} {window");
    assert_eq!(outer_title("{window}", "s", "a\x07\x1b]2;b"), "a]2;b");
    assert_eq!(outer_title("", "s", "w"), "");
}

/// The first title saves the host's, a change rewrites only the title, an
/// empty format hands the old one back, and so does the reset.
#[test]
fn outer_title_pushes_once_and_pops_on_reset() {
    let _guard = reset_lock();
    *OUTER_TITLE.lock().expect("title") = None;
    let mut out = Vec::new();
    sync_outer_title(&mut out, "s: a").expect("write");
    sync_outer_title(&mut out, "s: a").expect("write");
    sync_outer_title(&mut out, "s: b").expect("write");
    assert_eq!(out, b"\x1b[22;2t\x1b]2;s: a\x07\x1b]2;s: b\x07");
    out.clear();
    sync_outer_title(&mut out, "").expect("write");
    sync_outer_title(&mut out, "").expect("write");
    assert_eq!(out, b"\x1b[23;2t");
    out.clear();
    sync_outer_title(&mut out, "s: c").expect("write");
    write_terminal_reset(&mut out).expect("reset");
    assert!(out.starts_with(b"\x1b[22;2t\x1b]2;s: c\x07"), "{out:?}");
    assert!(out.ends_with(b"\x1b[23;2t"), "{out:?}");
}
