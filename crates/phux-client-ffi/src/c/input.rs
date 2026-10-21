//! Terminal input: keys, mouse, focus, and paste.

use std::mem;

use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};

use super::{
    BridgeError, PhuxClient, PhuxClientResult, PhuxKeyEvent, PhuxMouseEvent, PhuxResourceId,
    apply_input, check_struct, outbound_bytes_in, terminal_id_in, with_client_mut,
};

/// Sends a key event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and `event`
/// must be readable. Any non-empty satellite host span and any non-empty
/// `event.text` span selected by `event.has_text` must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_key(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    event: *const PhuxKeyEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("event is null"))?;
        check_struct(event.size, mem::size_of::<PhuxKeyEvent>(), event.version)?;
        let event = unsafe { key_input_event(event) }?;
        apply_input(client, &terminal_id, &event)
    })
}

/// True when the text carries a control codepoint, DEL, or a codepoint from
/// the private-use block platforms map their function keys into.
fn is_forbidden_key_text(text: &str) -> bool {
    text.chars()
        .any(|ch| ch <= '\u{1f}' || ch == '\u{7f}' || ('\u{f700}'..='\u{f8ff}').contains(&ch))
}

/// Reads the committed text a key event carries, if it carries any.
///
/// # Safety
///
/// Any non-empty `event.text` span selected by `event.has_text` must be
/// readable for the call.
unsafe fn key_event_text(event: &PhuxKeyEvent) -> Result<Option<String>, BridgeError> {
    if !event.has_text {
        if event.text.len != 0 {
            return Err(BridgeError::invalid(
                "key text bytes present when has_text is false",
            ));
        }
        return Ok(None);
    }
    let bytes = unsafe { outbound_bytes_in(event.text.data, event.text.len, "key text") }?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("key text is not UTF-8"))?;
    if is_forbidden_key_text(text) {
        return Err(BridgeError::invalid(
            "key text contains forbidden control or platform function codepoints",
        ));
    }
    Ok(Some(text.to_owned()))
}

/// Resolves the held and consumed modifier sets, rejecting unknown bits and a
/// consumed set that is not a subset of the held one.
fn key_event_modifiers(event: &PhuxKeyEvent) -> Result<(ModSet, ModSet), BridgeError> {
    let mods = ModSet::from_bits(event.modifiers)
        .ok_or_else(|| BridgeError::invalid("unknown key modifier bits"))?;
    let consumed_mods = ModSet::from_bits(event.consumed_modifiers)
        .ok_or_else(|| BridgeError::invalid("unknown consumed modifier bits"))?;
    if !mods.contains(consumed_mods) {
        return Err(BridgeError::invalid(
            "consumed modifiers are not a subset of modifiers",
        ));
    }
    Ok((mods, consumed_mods))
}

/// Resolves the unshifted codepoint a key event carries, if it carries one.
fn key_event_unshifted_codepoint(event: &PhuxKeyEvent) -> Result<Option<u32>, BridgeError> {
    if !event.has_unshifted_codepoint {
        if event.unshifted_codepoint != 0 {
            return Err(BridgeError::invalid(
                "unshifted codepoint is present without its discriminator",
            ));
        }
        return Ok(None);
    }
    char::from_u32(event.unshifted_codepoint)
        .ok_or_else(|| BridgeError::invalid("unshifted codepoint is not a Unicode scalar"))?;
    Ok(Some(event.unshifted_codepoint))
}

/// Builds the key input event a validated `PhuxKeyEvent` describes.
///
/// # Safety
///
/// Any non-empty `event.text` span selected by `event.has_text` must be
/// readable for the call.
unsafe fn key_input_event(event: &PhuxKeyEvent) -> Result<InputEvent, BridgeError> {
    let text = unsafe { key_event_text(event) }?;
    let (mods, consumed_mods) = key_event_modifiers(event)?;
    let unshifted_codepoint = key_event_unshifted_codepoint(event)?;
    Ok(InputEvent::Key(KeyEvent {
        action: KeyAction::try_from(event.action)
            .map_err(|_| BridgeError::invalid("unknown key action"))?,
        key: PhysicalKey::try_from(event.key)
            .map_err(|_| BridgeError::invalid("unknown physical key"))?,
        mods,
        consumed_mods,
        composing: event.composing,
        text,
        unshifted_codepoint,
    }))
}

/// Sends a mouse event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id`, `event`, and
/// any non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_mouse(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    event: *const PhuxMouseEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("event is null"))?;
        check_struct(event.size, mem::size_of::<PhuxMouseEvent>(), event.version)?;
        if !event.x.is_finite() || !event.y.is_finite() || event.x < 0.0 || event.y < 0.0 {
            return Err(BridgeError::invalid(
                "mouse coordinates must be finite and non-negative",
            ));
        }
        let input = InputEvent::Mouse(MouseEvent {
            action: MouseAction::try_from(event.action)
                .map_err(|_| BridgeError::invalid("unknown mouse action"))?,
            button: MouseButton::try_from(event.button)
                .map_err(|_| BridgeError::invalid("unknown mouse button"))?,
            mods: ModSet::from_bits(event.modifiers)
                .ok_or_else(|| BridgeError::invalid("unknown mouse modifier bits"))?,
            x: event.x,
            y: event.y,
        });
        apply_input(client, &terminal_id, &input)
    })
}

/// Sends a focus event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_focus(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    focused: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        apply_input(
            client,
            &terminal_id,
            &InputEvent::Focus(if focused {
                FocusEvent::Gained
            } else {
                FocusEvent::Lost
            }),
        )
    })
}

/// Sends pasted bytes to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable. When `len` is nonzero,
/// `data` must be readable for `len` bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_paste(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    data: *const u8,
    len: usize,
    trusted: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let data = unsafe { outbound_bytes_in(data, len, "paste data") }?;
        apply_input(
            client,
            &terminal_id,
            &InputEvent::Paste(PasteEvent {
                trust: if trusted {
                    PasteTrust::Trusted
                } else {
                    PasteTrust::Untrusted
                },
                data: data.to_vec(),
            }),
        )
    })
}
