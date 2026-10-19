use super::fit::{admit_delayed_fit, capacity, cell_px, pending_fit};
use super::typing::echo_guess;
use super::{
    Admit, HeldPaste, Identity, InputError, InputMetrics, admit_presentation,
    paste_needs_confirmation, upgrade_client,
};
use gpui::{Bounds, point, px, size};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

pub(super) fn metrics(width: f32, height: f32) -> InputMetrics {
    let bounds = Bounds::new(point(px(0.), px(0.)), size(px(width), px(height)));
    InputMetrics {
        bounds,
        cell_width: px(8.),
        line_height: px(17.),
        scale: 1.,
        cursor_bounds: bounds,
        shift: px(0.),
    }
}

#[test]
fn fit_skips_a_matching_grid_at_the_default_cell() {
    assert_eq!(pending_fit(None, (80, 24), (8, 16), (80, 24)), None);
}

#[test]
fn fit_reports_a_nondefault_cell_and_a_changed_pitch() {
    assert_eq!(
        pending_fit(None, (80, 24), (9, 20), (80, 24)),
        Some(((80, 24), (9, 20)))
    );
    assert_eq!(
        pending_fit(Some(((80, 24), (8, 16))), (80, 24), (9, 20), (80, 24)),
        Some(((80, 24), (9, 20)))
    );
    assert_eq!(
        pending_fit(Some(((80, 24), (9, 20))), (80, 24), (9, 20), (10, 4)),
        None
    );
}

#[test]
fn fit_reports_a_different_capacity_at_the_default_cell() {
    assert_eq!(
        pending_fit(None, (100, 40), (8, 16), (80, 24)),
        Some(((100, 40), (8, 16)))
    );
}

#[test]
fn cell_pitch_rounds_the_painted_cell() {
    let mut sample = metrics(800., 400.);
    sample.cell_width = px(9.5);
    sample.line_height = px(17.3);
    assert_eq!(cell_px(sample), (10, 17));
}

#[test]
fn capacity_counts_whole_cells_only() {
    assert_eq!(capacity(metrics(807., 409.)), Some((100, 24)));
    assert_eq!(capacity(metrics(16., 17.)), Some((2, 1)));
}

#[test]
fn collapsed_or_degenerate_panes_propose_nothing() {
    assert_eq!(capacity(metrics(15., 400.)), None);
    assert_eq!(capacity(metrics(400., 16.)), None);
    assert_eq!(capacity(metrics(f32::NAN, 400.)), None);
}

fn grid(terminal: phux_protocol::ResourceId, stream: u64, bootstrap: u64) -> super::GridFrame {
    use phux_client_runtime::publication::{
        Cursor, FrameColors, GridBuffer, GridDamage, Scrollbar,
    };
    super::GridFrame {
        terminal_id: terminal,
        generation: 1,
        stream_id: stream,
        bootstrap_id: bootstrap,
        last_seq: 0,
        cols: 1,
        rows: 1,
        cursor: Cursor::default(),
        scrollbar: Scrollbar::default(),
        colors: FrameColors::default(),
        damage: GridDamage::Full,
        buffer: GridBuffer::default(),
    }
}

#[test]
fn presentation_identity_rejects_the_wrong_terminal_and_a_replaced_replica() {
    let bound = phux_protocol::ResourceId::local(7);
    let frame = grid(bound.clone(), 1, 1);
    let Admit::Accept(first) = admit_presentation(&bound, None, 3, &frame) else {
        panic!("a first presentation must arm");
    };
    assert_eq!(
        admit_presentation(&bound, Some(first), 3, &frame),
        Admit::Accept(first)
    );
    assert_eq!(
        admit_presentation(&phux_protocol::ResourceId::local(8), Some(first), 3, &frame),
        Admit::WrongTerminal
    );
    assert_eq!(
        admit_presentation(&bound, Some(first), 4, &frame),
        Admit::Replaced
    );
    let swapped = grid(bound.clone(), 1, 2);
    assert_eq!(
        admit_presentation(&bound, Some(first), 3, &swapped),
        Admit::Replaced
    );
}

#[test]
fn only_a_line_break_typed_as_keys_or_a_bracket_end_needs_confirmation() {
    assert!(paste_needs_confirmation(false, "make\nmake install"));
    assert!(paste_needs_confirmation(false, "rm -rf build\r"));
    assert!(!paste_needs_confirmation(false, "one line"));
    assert!(!paste_needs_confirmation(true, "make\nmake install"));
    assert!(paste_needs_confirmation(true, "x\x1b[201~rm -rf ~\n"));
    assert!(paste_needs_confirmation(false, "\x1b[201~"));
}

#[test]
fn a_held_paste_counts_lines_and_never_prints_its_text() {
    let held = HeldPaste {
        text: "secret\nmore".into(),
    };
    assert_eq!(held.lines(), 2);
    assert_eq!(HeldPaste { text: "\n".into() }.lines(), 1);
    assert_eq!(format!("{held:?}"), "HeldPaste(11 bytes)");
}

#[test]
fn a_dropped_client_is_a_stale_handle() {
    let client = std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
        phux_client_runtime::control::ControlOptions::default(),
    ));
    let weak = std::sync::Arc::downgrade(&client);
    assert!(upgrade_client(&weak).is_ok());
    drop(client);
    assert_eq!(upgrade_client(&weak).unwrap_err(), InputError::StaleHandle);
}

fn press(key: PhysicalKey, mods: ModSet, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(str::to_owned),
        unshifted_codepoint: None,
    }
}

#[test]
fn local_echo_guesses_typed_text_and_skips_commands() {
    assert_eq!(
        echo_guess(&press(
            PhysicalKey::Unidentified,
            ModSet::empty(),
            Some("é")
        )),
        Some("é")
    );
    assert_eq!(
        echo_guess(&press(PhysicalKey::Enter, ModSet::empty(), None)),
        Some("\r")
    );
    assert_eq!(
        echo_guess(&press(PhysicalKey::Backspace, ModSet::SHIFT, None)),
        Some("\u{7f}")
    );
    assert_eq!(
        echo_guess(&press(PhysicalKey::C, ModSet::CTRL, Some("c"))),
        None
    );
    assert_eq!(
        echo_guess(&press(PhysicalKey::ArrowLeft, ModSet::empty(), None)),
        None
    );
    let mut release = press(PhysicalKey::Unidentified, ModSet::empty(), Some("a"));
    release.action = KeyAction::Release;
    assert_eq!(echo_guess(&release), None);
}

fn generation(epoch: u64) -> Identity {
    Identity {
        epoch,
        stream: 1,
        bootstrap: 1,
    }
}

/// Pressing another pane, switching tabs, or zooming a sibling moves the
/// visible-owner claim. The hidden view's token no longer matches.
#[test]
fn press_tab_switch_and_zoom_retire_a_hidden_owners_fit() {
    let armed = generation(1);
    assert!(
        !admit_delayed_fit(Some(2), 1, armed, Some(armed)),
        "the focused writable view holds the claim"
    );
    assert!(
        !admit_delayed_fit(None, 1, armed, Some(armed)),
        "a painted non-owner released the claim"
    );
}

/// Blur and occlusion leave the same view as the visible owner, and
/// coming back to it on the armed generation may settle.
#[test]
fn the_visible_owner_on_its_armed_generation_is_admitted() {
    let armed = generation(4);
    assert!(admit_delayed_fit(Some(7), 7, armed, Some(armed)));
}

#[test]
fn reconnect_replaces_the_armed_generation() {
    let armed = generation(1);
    assert!(!admit_delayed_fit(Some(7), 7, armed, Some(generation(2))));
    assert!(!admit_delayed_fit(Some(7), 7, armed, None));
}
