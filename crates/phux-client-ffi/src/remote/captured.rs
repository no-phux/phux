//! Independent dials from one captured registry capability, without resolving
//! the alias again or sharing an active transport. Token bytes stay in the pump.
use std::ptr;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use super::{PhuxRemoteTunnel, REMOTE_TUNNEL_RESOLVED, Shared, guard};
use crate::error::BridgeError;
use crate::types::PhuxClientResult;

impl PhuxRemoteTunnel {
    fn clone_resolved(&self) -> Result<Self, BridgeError> {
        let resolved = self
            .resolved
            .clone()
            .ok_or_else(|| BridgeError::state("remote tunnel has no resolved host"))?;
        Ok(Self {
            resolved: Some(resolved),
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            session: self.session.clone(),
            shared: Arc::new(Shared::with_state(REMOTE_TUNNEL_RESOLVED)),
            cancel: Arc::new(Notify::new()),
            thread: Mutex::new(None),
        })
    }
}

/// Copy the captured dial configuration into a fresh RESOLVED tunnel.
///
/// The endpoint, pin and token-file/config provenance are immutable and copied
/// exactly. No registry or token file is read, and no active connection, socket,
/// thread or cancellation state is shared. A prior dial failure does not revoke
/// the captured configuration. Explicit registry Retry should resolve anew.
///
/// # Safety
///
/// `source` must be live throughout the call (no concurrent free), and `out_tunnel`
/// writable. May run concurrently with source start/info. Caller owns the new
/// handle and must free it once. A writable output is cleared on every failure.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_remote_tunnel_clone_resolved(
    source: *const PhuxRemoteTunnel,
    out_tunnel: *mut *mut PhuxRemoteTunnel,
) -> PhuxClientResult {
    guard(|| {
        // SAFETY: checked before write.
        let out = unsafe { out_tunnel.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_tunnel is null"))?;
        *out = ptr::null_mut();
        // SAFETY: checked before dereference; the source's dial config is immutable.
        let source = unsafe { source.as_ref() }
            .ok_or_else(|| BridgeError::invalid("source tunnel is null"))?;
        *out = Box::into_raw(Box::new(source.clone_resolved()?));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    #[test]
    fn clone_keeps_exact_endpoint_pin_and_token_provenance_after_registry_edit() {
        let dir = tempfile::tempdir().expect("fixture");
        let config = dir.path().join("config.toml");
        let token = dir.path().join("token-A");
        std::fs::write(&config, format!("[[remote]]\nname='mini'\nendpoint='wss://endpoint-A:8788'\ntoken-file='{}'\ncert-fingerprint='pin-A'\nsession='work'\n", token.display())).expect("config");
        let source = PhuxRemoteTunnel::resolve("mini", Some(&config));
        let original = source.resolved.as_ref().expect("resolved").clone();
        assert_eq!(original.cert_fingerprint.as_deref(), Some("pin-A"));
        assert_eq!(original.token_file.as_deref(), Some(token.as_path()));
        std::fs::write(
            &config,
            "[[remote]]\nname='mini'\nendpoint='ws://endpoint-B:8788'\n",
        )
        .expect("rewrite");
        source.shared.fail("old dial failed".into());
        let mut cloned = ptr::null_mut();
        // SAFETY: live source and writable output; owns cloned handle below.
        assert_eq!(
            unsafe { phux_remote_tunnel_clone_resolved(&raw const source, &raw mut cloned) },
            PhuxClientResult::Ok
        );
        // SAFETY: successful clone returned a unique allocated handle.
        let cloned = unsafe { Box::from_raw(cloned) };
        assert_eq!(cloned.resolved.as_ref(), Some(&original));
        assert_eq!(
            cloned.shared.state.load(Ordering::Acquire),
            REMOTE_TUNNEL_RESOLVED
        );
        assert!(!Arc::ptr_eq(&source.shared, &cloned.shared));
        assert!(!Arc::ptr_eq(&source.cancel, &cloned.cancel));
        assert!(cloned.thread.lock().expect("thread mutex").is_none());
        drop(source);
        assert_eq!(cloned.endpoint, "wss://endpoint-A:8788");
        assert_eq!(cloned.session, "work");
    }

    #[test]
    fn invalid_clone_clears_output_without_network_or_registry_fallback() {
        let mut out = ptr::dangling_mut();
        // SAFETY: null source is the rejection case; output is writable.
        assert_eq!(
            unsafe { phux_remote_tunnel_clone_resolved(ptr::null(), &raw mut out) },
            PhuxClientResult::InvalidArgument
        );
        assert!(out.is_null());
        let dir = tempfile::tempdir().expect("fixture");
        let source = PhuxRemoteTunnel::resolve("unknown", Some(&dir.path().join("absent.toml")));
        out = ptr::dangling_mut();
        // SAFETY: live unresolved source and writable output.
        assert_eq!(
            unsafe { phux_remote_tunnel_clone_resolved(&raw const source, &raw mut out) },
            PhuxClientResult::InvalidState
        );
        assert!(out.is_null());
        // SAFETY: null output is the rejection case; source remains live.
        assert_eq!(
            unsafe { phux_remote_tunnel_clone_resolved(&raw const source, ptr::null_mut()) },
            PhuxClientResult::InvalidArgument
        );
    }
}
