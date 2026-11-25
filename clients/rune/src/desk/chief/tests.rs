use phux_client_runtime::control::{AgentSessionDescriptor, PaneDescriptor};
use phux_protocol::native_agent::NativeCapabilities;
use rune::{Env, Mods, PhysicalInput, Runtime};

use super::*;
use crate::desk::input::Modal;

fn pane(n: u32, session: &str) -> PaneDescriptor {
    PaneDescriptor {
        terminal_id: ResourceId::local(n),
        session_id: n,
        session_name: session.to_owned(),
        cols: 80,
        rows: 24,
        title: None,
        cwd: None,
        is_focused: false,
        key: None,
        revision: None,
        name: None,
        activation: None,
        exec: None,
        foreground: None,
        icon: None,
        parent: None,
    }
}

fn agent(id: u32, parent: u32) -> AgentSessionDescriptor {
    AgentSessionDescriptor {
        id: ResourceId::local(id),
        parent: Some(ResourceId::local(parent)),
        provider: Some("opencode".into()),
        native_id: None,
        state: None,
    }
}

fn view(connected: bool, prompts: bool) -> NativeView {
    NativeView {
        activation: "activation-C".into(),
        backend: "acp".into(),
        capabilities: NativeCapabilities {
            prompt: prompts.into(),
            ..NativeCapabilities::default()
        },
        connected,
        turn_id: None,
        provider_session_id: None,
        questions: vec![],
    }
}

#[test]
fn the_tap_names_a_session_or_turns_off() {
    assert_eq!(session(None), Some(DEFAULT_SESSION));
    assert_eq!(session(Some("  ")), Some(DEFAULT_SESSION));
    assert_eq!(session(Some(" staff ")), Some("staff"));
    assert_eq!(session(Some("None")), None);
}

#[test]
fn only_a_connected_prompting_acp_agent_in_the_named_session_is_a_target() {
    let mut topology = crate::desk::topology(vec![], vec![pane(1, "work"), pane(2, "chief")]);
    let missing = target(Some(&topology), "staff", |_| None).unwrap_err();
    assert!(missing.starts_with("No session named ‘staff’"), "{missing}");

    // A plain terminal is never typed into.
    let plain = target(Some(&topology), "chief", |_| None).unwrap_err();
    assert!(plain.contains("never typed into"), "{plain}");

    // An agent in another session is not the chief.
    topology.agent_sessions = vec![agent(10, 1)];
    let elsewhere = |_: &ResourceId| Some(view(true, true));
    assert!(target(Some(&topology), "chief", elsewhere).is_err());

    topology.agent_sessions.push(agent(11, 2));
    let unsupported = target(Some(&topology), "chief", |_| Some(view(true, false)));
    assert!(unsupported.unwrap_err().contains("takes no prompts"));
    let gone = target(Some(&topology), "chief", |_| Some(view(false, true)));
    assert!(gone.unwrap_err().contains("no terminal fallback"));
    let quiet = target(Some(&topology), "chief", |_| None);
    assert!(quiet.unwrap_err().contains("hasn't reported"));

    let live = target(Some(&topology), "chief", |_| Some(view(true, true)));
    assert_eq!(live, Ok((ResourceId::local(11), "activation-C".to_owned())));
    assert!(target(None, "chief", |_| None).is_err());
}

#[test]
fn receipts_settle_only_our_prompt_and_never_resend() {
    let mut chief = Chief {
        draft: TextEdit::new("ship the fix"),
        pending: Some(7),
        ..Chief::default()
    };
    assert!(
        !chief.receipt(8, NativeDisposition::Completed, None),
        "not ours"
    );
    assert!(chief.receipt(
        7,
        NativeDisposition::Refused,
        Some("ACP turn already active")
    ));
    assert!(!chief.sending(), "a refusal frees the field");
    assert_eq!(
        chief.draft.text, "ship the fix",
        "a refusal keeps the draft"
    );
    assert_eq!(
        chief.error.as_deref(),
        Some("Refused. Nothing retried; draft preserved: ACP turn already active")
    );
    assert!(
        !chief.receipt(7, NativeDisposition::Completed, None),
        "settled once"
    );

    // An ACP prompt's only receipt is Queued: it was sent, and the field
    // is free for the next one (the server's own receipt, verbatim).
    chief.pending = Some(9);
    let scheduled = "ACP prompt scheduled; ACP v1 has no admission receipt";
    assert!(chief.receipt(9, NativeDisposition::Queued, Some(scheduled)));
    assert!(!chief.sending());
    assert!(chief.draft.text.is_empty(), "queued clears the draft");
    assert!(chief.error.is_none());

    chief.draft = TextEdit::new("again");
    chief.pending = Some(11);
    assert!(chief.receipt(11, NativeDisposition::Unknown, None));
    assert!(!chief.sending());
    assert_eq!(chief.draft.text, "again", "unknown keeps the draft");
    assert!(
        chief
            .error
            .as_deref()
            .is_some_and(|e| e.contains("never retried"))
    );

    chief.pending = Some(10);
    assert!(chief.disconnected());
    assert!(!chief.sending());
    assert!(
        chief
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Outcome unknown"))
    );
    assert!(!chief.disconnected(), "nothing left on its way");
}

#[test]
fn sending_without_a_chief_says_why_and_sends_nothing() {
    let mut desk = crate::test_support::window();
    desk.chief.draft = TextEdit::new("   ");
    desk.send_chief();
    assert!(
        desk.chief
            .error
            .as_deref()
            .is_some_and(|e| e.contains("first"))
    );
    desk.chief.draft = TextEdit::new("triage the red CI");
    desk.send_chief();
    assert_eq!(
        desk.chief.error.as_deref(),
        Some("Not connected. Nothing was sent.")
    );
    assert!(!desk.chief.sending());
}

fn runtime() -> Runtime<Desk> {
    let mut desk = crate::test_support::window();
    desk.focused = true;
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, (900.0, 700.0), Env::new(true, 1.0));
    rt.on_key = Some(Desk::key);
    // As the app builds its window (`lib.rs`).
    rt.set_modifier_keys(true);
    rt.frame(0.0);
    rt
}

fn physical(rt: &mut Runtime<Desk>, key: PhysicalKeyCode, pressed: bool, mods: Mods) {
    rt.physical_input(PhysicalInput::Key {
        key,
        mods,
        pressed,
        repeat: false,
    });
    rt.frame(0.0);
}

fn right_command(rt: &mut Runtime<Desk>, pressed: bool) {
    let mods = Mods {
        cmd: pressed,
        logo: pressed,
        ..Mods::default()
    };
    physical(rt, PhysicalKeyCode::SuperRight, pressed, mods);
}

#[test]
fn a_right_command_tap_opens_the_composer_and_escape_keeps_the_draft() {
    let mut rt = runtime();
    right_command(&mut rt, true);
    assert!(!rt.state.chief.open, "a press alone opens nothing");
    right_command(&mut rt, false);
    assert!(rt.state.chief.open);
    assert_eq!(rt.state.modal(), Some(Modal::Chief));
    assert!(
        rt.find("chief-draft").is_some(),
        "the release built the composer"
    );
    assert!(
        rt.has_focus(),
        "the field takes the keyboard, so dictation lands in it"
    );

    // A dictation app pastes what it heard into the focused field, never
    // the terminal behind it.
    rt.paste("dictated words");
    rt.frame(0.1);
    assert_eq!(rt.state.chief.draft.text, "dictated words");
    rt.key(KeyEvent {
        key: Key::Escape,
        mods: Mods::default(),
    });
    rt.frame(0.1);
    assert!(!rt.state.chief.open);
    assert_eq!(rt.state.chief.draft.text, "dictated words");

    // Over the open composer, the key does nothing: a dictation app may be
    // holding it to listen again.
    rt.update(|d, _| d.open_chief());
    right_command(&mut rt, true);
    right_command(&mut rt, false);
    assert!(rt.state.chief.open);
}

#[test]
fn a_chord_a_click_a_left_command_or_focus_loss_is_not_a_tap() {
    let mut rt = runtime();
    // Right Command + K: a chord.
    right_command(&mut rt, true);
    let held = Mods {
        cmd: true,
        logo: true,
        ..Mods::default()
    };
    physical(&mut rt, PhysicalKeyCode::KeyK, true, held);
    rt.key(KeyEvent {
        key: Key::Text("k".into()),
        mods: held,
    });
    physical(&mut rt, PhysicalKeyCode::KeyK, false, held);
    right_command(&mut rt, false);
    assert!(!rt.state.chief.open, "a chord opened the composer");

    // A menu pick while it is held (the menu takes the chord's key).
    right_command(&mut rt, true);
    rt.update(|d, tx| d.shortcut(crate::keys::Shortcut::ZoomReset, tx));
    right_command(&mut rt, false);
    assert!(!rt.state.chief.open, "a menu pick opened the composer");

    // A click on a pane while it is held: ⌘-click opens links.
    right_command(&mut rt, true);
    rt.update(|d, _| d.chief.disarm());
    right_command(&mut rt, false);
    assert!(!rt.state.chief.open);

    // Control is down first: a chord, not a tap.
    let ctrl = Mods {
        ctrl: true,
        ..Mods::default()
    };
    let both = Mods {
        cmd: true,
        logo: true,
        ..ctrl
    };
    physical(&mut rt, PhysicalKeyCode::ControlLeft, true, ctrl);
    physical(&mut rt, PhysicalKeyCode::SuperRight, true, both);
    physical(&mut rt, PhysicalKeyCode::SuperRight, false, ctrl);
    physical(
        &mut rt,
        PhysicalKeyCode::ControlLeft,
        false,
        Mods::default(),
    );
    assert!(
        !rt.state.chief.open,
        "ctrl+right command opened the composer"
    );

    // The left Command key is never the chief's.
    physical(&mut rt, PhysicalKeyCode::SuperLeft, true, held);
    physical(&mut rt, PhysicalKeyCode::SuperLeft, false, Mods::default());
    assert!(!rt.state.chief.open);

    // Focus loss releases the held key with no physical key-up.
    right_command(&mut rt, true);
    rt.physical_input(PhysicalInput::FocusLost);
    rt.frame(0.0);
    assert!(!rt.state.chief.open, "Cmd-Tab away opened the composer");

    // Turned off in the file, the tap does nothing.
    rt.update(|d, _| d.file.chief = Some("none".into()));
    right_command(&mut rt, true);
    right_command(&mut rt, false);
    assert!(!rt.state.chief.open);
}
