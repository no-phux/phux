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
    viewport: ((f32, f32), f32),
    poisoned: bool,
}

#[unsafe(no_mangle)]
pub extern "C" fn phux_rune_ios_prewarm() {
    attempt(|| {
        rune::embed::prewarm();
        Ok(())
    });
}

/// Only a simulator qualification probe supplies this private output path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_capture_connection_diagnostics(path: *const c_char) -> bool {
    attempt(|| {
        #[cfg(target_abi = "sim")]
        {
            super::diagnostics::start(std::path::Path::new(unsafe { text(path)? }))?;
            Ok(true)
        }
        #[cfg(not(target_abi = "sim"))]
        {
            let _ = path;
            Err("Connection diagnostics are available only to simulator qualification.".to_owned())
        }
    })
}

/// Simulator fixture only: stop capture and publish its sticky health receipt.
#[unsafe(no_mangle)]
pub extern "C" fn phux_rune_ios_finish_connection_diagnostics() -> bool {
    attempt(|| {
        #[cfg(target_abi = "sim")]
        {
            super::diagnostics::finish()?;
            Ok(true)
        }
        #[cfg(not(target_abi = "sim"))]
        Err("Connection diagnostics are available only to simulator qualification.".to_owned())
    })
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
            viewport: ((width as f32, height as f32), scale as f32),
            poisoned: false,
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
        let host = unsafe { host(pointer)? };
        let viewport = ((width as f32, height as f32), scale as f32);
        if host.viewport != viewport {
            mobile::viewport_changed(host.layer.runtime_mut());
            host.viewport = viewport;
        }
        host.layer.resize(viewport.0, viewport.1);
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
        let ground = mobile::edge_srgb(host.layer.runtime());
        // Only a drawn frame moves them; otherwise UIKit keeps the last.
        let controls = frame.drew.then(|| mobile::controls(host.layer.runtime()));
        string(
            json!({"drew":frame.drew,"animating":frame.animating || reconciled,
            "wake_after_secs":frame.wake_after_secs,"input":input,"requests":requests,
            "ground":ground,"controls":controls}),
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

/// A tap on the native control `id` names: true when it ran its press.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_press(pointer: *mut c_void, id: *const c_char) -> bool {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        Ok(mobile::press(host.layer.runtime_mut(), unsafe {
            text(id)?
        }))
    })
}

/// The native long-press menu picked verb `id`: true when it named one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_menu_pick(
    pointer: *mut c_void,
    request: u64,
    id: *const c_char,
) -> bool {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        Ok(mobile::menu_pick(
            host.layer.runtime_mut(),
            request,
            unsafe { text(id)? },
        ))
    })
}

/// The native menu disappeared; a selected correlated action may still follow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_menu_dismiss(pointer: *mut c_void, request: u64) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        mobile::menu_dismiss(host.layer.runtime_mut(), request);
        Ok(())
    });
}

/// A tapped card, widget or Dynamic Island: true when the link was one.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_open_link(pointer: *mut c_void, url: *const c_char) -> bool {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        Ok(mobile::open_link(host.layer.runtime_mut(), unsafe {
            text(url)?
        }))
    })
}

/// Words heard for `session` by the Action Button's dictation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_dictate(
    pointer: *mut c_void,
    session: *const c_char,
    words: *const c_char,
) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        let (session, words) = unsafe { (text(session)?, text(words)?) };
        mobile::dictate(host.layer.runtime_mut(), session, words);
        Ok(())
    });
}

/// The phone's Live Activity gateway and grant, for every computer it attaches to.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_set_live(
    pointer: *mut c_void,
    device: *const c_char,
    gateway: *const c_char,
    grant: *const c_char,
) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        let (device, gateway, grant) = unsafe { (text(device)?, text(gateway)?, text(grant)?) };
        mobile::set_live(host.layer.runtime_mut(), device, gateway, grant);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_paste(pointer: *mut c_void, pasted: *const c_char) {
    attempt(|| {
        // Oversized or invalid text is refused aloud, not silently dropped.
        let pasted = unsafe { text(pasted) }.ok();
        mobile::paste(unsafe { host(pointer)? }.layer.runtime_mut(), pasted);
        Ok(())
    });
}

/// One picked file for attachment request `id`; null `data` when UIKit
/// couldn't read it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_attach(
    pointer: *mut c_void,
    id: u64,
    extension: *const c_char,
    data: *const u8,
    length: usize,
) {
    attempt(|| {
        let host = unsafe { host(pointer)? };
        let extension = unsafe { text(extension)? };
        // SAFETY: Bridge.h requires `length` readable bytes for this call.
        let data =
            (!data.is_null()).then(|| unsafe { std::slice::from_raw_parts(data, length) }.to_vec());
        mobile::attach(host.layer.runtime_mut(), id, extension, data);
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_path_changed(pointer: *mut c_void) {
    attempt(|| {
        mobile::path_changed(unsafe { host(pointer)? }.layer.runtime_mut());
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_zoom(pointer: *mut c_void, larger: bool) {
    attempt(|| {
        mobile::zoom(unsafe { host(pointer)? }.layer.runtime_mut(), larger);
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
    increased_contrast: bool,
    reduce_transparency: bool,
    content_size: *const c_char,
) {
    attempt(|| {
        // An unreadable category reads as the default size.
        let content_size = unsafe { text(content_size) }.unwrap_or_default();
        let rt = unsafe { host(pointer)? }.layer.runtime_mut();
        rt.set_system_dark(dark);
        rt.set_system_reduced_motion(reduced_motion);
        rt.set_system_increased_contrast(increased_contrast);
        mobile::set_accessibility(rt, reduce_transparency, content_size);
        Ok(())
    });
}

/// Records `input_ready` on the attempt clock, immediately before the probe types.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_trace_input_ready(pointer: *mut c_void) {
    attempt(|| {
        mobile::trace_input_ready(unsafe { host(pointer)? }.layer.runtime());
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
