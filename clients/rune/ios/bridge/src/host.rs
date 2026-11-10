//! The UIKit-owned layer and a main-thread Rune host. The Swift owner
//! retains its layer until destroy, and a separately retained WakeBox
//! outlives every worker callback, even when a connection is replaced.
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use phux_rune::mobile::{self, Input, Operation, State, rune};
use rune::embed::LayerHost;
use serde_json::{Value, json};

type Callback = extern "C" fn(*mut c_void);
thread_local! { static ERROR: RefCell<String> = const { RefCell::new(String::new()) }; }

struct Wake {
    context: usize,
    wake: Callback,
    release: Callback,
}
impl Drop for Wake {
    fn drop(&mut self) {
        (self.release)(self.context as *mut c_void);
    }
}

struct Host {
    layer: LayerHost<State>,
    input: Input,
    frames: u64,
    poisoned: bool,
    dark: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn phux_rune_ios_prewarm() {
    attempt(|| {
        rune::embed::prewarm();
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_validate(configuration: *const c_char) -> *mut c_char {
    attempt(|| {
        let configuration = unsafe { parse_configuration(configuration)? };
        configuration.validate_connection()?;
        string(json!({"valid":true}))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_pairing_import(invite: *const c_char) -> *mut c_char {
    attempt(|| {
        // Borrowed only for this call; the returned JSON is bridge-owned and
        // released through phux_rune_ios_free, just like validation responses.
        let invite = unsafe { text(invite)? };
        string(mobile::parse_pairing_invite(invite)?)
    })
}

pub(super) fn attempt<T: Default>(f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(result)) => result,
        result => {
            let error = match result {
                Ok(Err(error)) => error,
                _ => "The native Rune operation failed.".into(),
            };
            ERROR.with_borrow_mut(|slot| *slot = error);
            T::default()
        }
    }
}

pub(super) fn string(value: Value) -> Result<*mut c_char, String> {
    CString::new(value.to_string())
        .map(CString::into_raw)
        .map_err(|e| e.to_string())
}

pub(super) unsafe fn text<'a>(pointer: *const c_char) -> Result<&'a str, String> {
    if pointer.is_null() {
        return Err("Missing native string.".into());
    }
    // SAFETY: Bridge.h requires a borrowed NUL-terminated string for this call.
    let bytes = unsafe { CStr::from_ptr(pointer) }.to_bytes();
    if bytes.len() > 1024 * 1024 {
        return Err("Native input exceeds 1 MiB.".into());
    }
    std::str::from_utf8(bytes).map_err(|_| "Native input is not UTF-8.".into())
}

pub(super) unsafe fn parse_configuration(
    pointer: *const c_char,
) -> Result<mobile::Configuration, String> {
    // Do not echo serde diagnostics: an unknown field name or invalid value
    // may contain the user's ticket or token.
    serde_json::from_str(unsafe { text(pointer)? })
        .map_err(|_| "Invalid native host configuration.".into())
}

unsafe fn host<'a>(pointer: *mut c_void) -> Result<&'a mut Host, String> {
    if pointer.is_null() {
        return Err("The native host is not open.".into());
    }
    // SAFETY: the opaque pointer is returned by create, exclusively used on
    // the main thread, and not used again after destroy (Bridge.h).
    let host = unsafe { &mut *pointer.cast::<Host>() };
    if host.poisoned {
        return Err("The native host stopped after a frame failure.".into());
    }
    Ok(host)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_create(
    layer: *mut c_void,
    width: f64,
    height: f64,
    scale: f64,
    dark: bool,
    configuration: *const c_char,
    context: *mut c_void,
    wake: Callback,
    release: Callback,
    identity: *mut c_void,
) -> *mut c_void {
    let callback = Arc::new(Wake {
        context: context as usize,
        wake,
        release,
    });
    attempt(|| {
        let configuration = unsafe { parse_configuration(configuration)? };
        let state = mobile::state_with_identity(configuration, unsafe {
            super::identity::optional_signer(identity)
        })?;
        let waker: rune::Waker = Arc::new(move || (callback.wake)(callback.context as *mut c_void));
        let layer = unsafe {
            LayerHost::from_core_animation_layer(
                layer,
                &mobile::spec(),
                state,
                (width as f32, height as f32),
                scale as f32,
                dark,
                waker,
            )
        }?;
        Ok(Box::into_raw(Box::new(Host {
            layer,
            input: Input::default(),
            frames: 0,
            poisoned: false,
            dark,
        }))
        .cast())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_destroy(pointer: *mut c_void) {
    if !pointer.is_null() {
        // SAFETY: exactly the live allocation returned by create, consumed once.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(pointer.cast::<Host>()))
        }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_resize(
    pointer: *mut c_void,
    width: f64,
    height: f64,
    scale: f64,
) {
    attempt(|| {
        if ![width, height, scale].iter().all(|v| v.is_finite()) {
            return Err("Non-finite viewport.".into());
        }
        unsafe { host(pointer)? }
            .layer
            .resize((width as f32, height as f32), scale as f32);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_frame(pointer: *mut c_void) -> *mut c_char {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        if host
            .layer
            .runtime()
            .state
            .native
            .as_ref()
            .is_none_or(|n| !n.active)
        {
            return string(json!({"drew":false,"animating":false,"wake_after_secs":null}));
        }
        host.poisoned = true;
        let frame = host.layer.frame();
        host.poisoned = false;
        if frame.drew {
            host.frames += 1;
            host.layer.runtime().state.drawn();
        }
        let reconciled = host.input.reconcile(host.layer.runtime_mut());
        let input = host.input.snapshot(host.layer.runtime());
        let requests = mobile::take_requests(host.layer.runtime_mut());
        let ground = mobile::edge_srgb(host.layer.runtime(), host.dark);
        string(
            json!({"drew":frame.drew,"animating":frame.animating || reconciled,
            "wake_after_secs":frame.wake_after_secs,"input":input,"requests":requests,
            "ground":ground}),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_input(
    pointer: *mut c_void,
    operation: *const c_char,
) -> *mut c_char {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        let operation = match unsafe { parse_input(operation) } {
            Ok(operation) => operation,
            Err(error) => return input_response(host, false, Some(error)),
        };
        // Invalid/stale edits are recoverable, not renderer failures. Return
        // the authoritative document before UIKit's next synchronous key.
        // A copy this makes leaves with the next frame's requests.
        let (accepted, rejected) = match host.input.apply(host.layer.runtime_mut(), operation) {
            Ok(accepted) => (accepted, None),
            Err(_) => (false, Some("This input edit is not supported.".into())),
        };
        input_response(host, accepted, rejected)
    })
}

unsafe fn parse_input(pointer: *const c_char) -> Result<Operation, String> {
    let raw = unsafe { text(pointer) }
        .map_err(|_| "The input is missing, invalid, or larger than 1 MiB.".to_owned())?;
    serde_json::from_str(raw).map_err(|_| "This input operation is not supported.".to_owned())
}

fn input_response(
    host: &mut Host,
    accepted: bool,
    error: Option<String>,
) -> Result<*mut c_char, String> {
    // Malformed/oversized input skips `apply`; retire its previous owner
    // before an authoritative snapshot can adopt a different document.
    host.input.reconcile(host.layer.runtime_mut());
    string(
        json!({"accepted":accepted,"input":host.input.snapshot(host.layer.runtime()),
        "error":error}),
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_touch(
    pointer: *mut c_void,
    identity: u64,
    phase: u32,
    x: f64,
    y: f64,
    seconds: f64,
) {
    attempt(|| {
        let phase = match phase {
            0 => rune::TouchPhase::Started,
            1 => rune::TouchPhase::Moved,
            2 => rune::TouchPhase::Ended,
            3 => rune::TouchPhase::Cancelled,
            _ => return Err("Invalid touch phase.".into()),
        };
        if ![x, y, seconds].iter().all(|v| v.is_finite()) {
            return Err("Non-finite touch.".into());
        }
        let host = unsafe { host(pointer)? };
        if host
            .layer
            .runtime()
            .state
            .native
            .as_ref()
            .is_some_and(|n| n.active)
        {
            host.layer
                .runtime_mut()
                .touch(identity, phase, x as f32, y as f32, seconds);
        }
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_paste(pointer: *mut c_void, pasted: *const c_char) {
    attempt(|| {
        let pasted = unsafe { text(pasted)? };
        mobile::paste(unsafe { host(pointer)? }.layer.runtime_mut(), pasted);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_active(pointer: *mut c_void, active: bool) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        host.input.cancel_preedit(host.layer.runtime_mut());
        host.input.retire();
        mobile::set_active(host.layer.runtime_mut(), active);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_traits(
    pointer: *mut c_void,
    dark: bool,
    reduced_motion: bool,
) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        host.dark = dark;
        let rt = host.layer.runtime_mut();
        rt.set_system_dark(dark);
        rt.set_system_reduced_motion(reduced_motion);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_report(
    pointer: *mut c_void,
    marker: *const c_char,
) -> *mut c_char {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        string(mobile::report(host.layer.runtime(), host.frames, unsafe {
            text(marker)?
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_tree(pointer: *mut c_void) -> *mut c_char {
    attempt(|| {
        string(
            unsafe { host(pointer)? }
                .layer
                .runtime()
                .accessibility_json(),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn phux_rune_ios_error() -> *mut c_char {
    attempt(|| string(json!({"error": ERROR.with_borrow(Clone::clone)})))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_free(pointer: *mut c_char) {
    if !pointer.is_null() {
        // SAFETY: one string returned by this bridge, consumed exactly once.
        unsafe {
            drop(CString::from_raw(pointer));
        }
    }
}
