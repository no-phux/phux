//! Security.framework owns the key; this adapter owns only its public point
//! and an independently retained, thread-safe signing callback context.
use std::ffi::{c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use phux_rune::mobile::{self, DeviceSigner};
use serde_json::json;

use super::host::{attempt, parse_configuration, string, text};

type Release = extern "C" fn(*mut c_void);
type Sign = extern "C" fn(*mut c_void, *const u8, usize, *mut u8, usize) -> isize;

struct PlatformSigner {
    point: Vec<u8>,
    // Not a Rust pointer: Swift owns the immutable SecKey context, guarantees
    // concurrent signing, and releases it only in Drop after the last Arc.
    context: usize,
    sign: Sign,
    release: Release,
}

impl Drop for PlatformSigner {
    fn drop(&mut self) {
        (self.release)(self.context as *mut c_void);
    }
}

impl DeviceSigner for PlatformSigner {
    fn public_point(&self) -> Vec<u8> {
        self.point.clone()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let mut signature = [0u8; 80];
        let length = (self.sign)(
            self.context as *mut c_void,
            message.as_ptr(),
            message.len(),
            signature.as_mut_ptr(),
            signature.len(),
        );
        let length = usize::try_from(length)
            .ok()
            .filter(|length| *length > 0 && *length <= signature.len())
            .ok_or("The platform-held device key could not sign.")?;
        Ok(signature[..length].to_vec())
    }
}

struct Identity(Arc<dyn DeviceSigner>);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_identity_create(
    point: *const u8,
    length: usize,
    context: *mut c_void,
    sign: Sign,
    release: Release,
) -> *mut c_void {
    let mut signer = PlatformSigner {
        point: Vec::new(),
        context: context as usize,
        sign,
        release,
    };
    attempt(|| {
        if point.is_null() || length != 65 {
            return Err("The device key is not a P-256 public point.".into());
        }
        // SAFETY: Bridge.h borrows the readable public point for this call.
        let bytes = unsafe { std::slice::from_raw_parts(point, length) };
        if bytes[0] != 4 {
            return Err("The device key is not an uncompressed P-256 public point.".into());
        }
        signer.point = bytes.to_vec();
        Ok(Box::into_raw(Box::new(Identity(Arc::new(signer)))).cast())
    })
}

pub(super) unsafe fn optional_signer(pointer: *mut c_void) -> Option<Arc<dyn DeviceSigner>> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: this live wrapper is borrowed; its Swift owner cannot destroy
    // it concurrently. Runtime dials own independent Arc clones thereafter.
    Some(Arc::clone(&unsafe { &*pointer.cast::<Identity>() }.0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_identity_destroy(pointer: *mut c_void) {
    if !pointer.is_null() {
        // SAFETY: one wrapper returned by identity_create, consumed once.
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe {
            drop(Box::from_raw(pointer.cast::<Identity>()))
        }));
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_identity_validate(
    config: *const c_char,
    identity: *mut c_void,
) -> *mut c_char {
    attempt(|| {
        unsafe { parse_configuration(config)? }
            .target_with_identity(unsafe { optional_signer(identity) })?;
        string(json!({"valid": true}))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_enroll(
    config: *const c_char,
    ticket: *const c_char,
    identity: *mut c_void,
) -> *mut c_char {
    attempt(|| {
        let configuration = unsafe { parse_configuration(config)? };
        let target = configuration.enrollment_target()?;
        let signer =
            unsafe { optional_signer(identity) }.ok_or("The device key is unavailable.")?;
        let ticket = unsafe { text(ticket)? };
        let enrolled = mobile::enroll_target_blocking(&target, ticket, signer)
            .map_err(|error| redact(error, ticket, configuration.token.as_deref()))?;
        string(json!({"chain_pem": enrolled.chain_pem}))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_rune_ios_enrollment_validate(config: *const c_char) -> *mut c_char {
    attempt(|| {
        unsafe { parse_configuration(config)? }.enrollment_target()?;
        string(json!({"valid": true}))
    })
}

fn redact(mut error: String, ticket: &str, token: Option<&str>) -> String {
    for secret in [Some(ticket), token]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
    {
        error = error.replace(secret, "[redacted]");
    }
    error
}
