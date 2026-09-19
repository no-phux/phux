//! Independent dials from one captured registry capability, without resolving
//! the alias again or sharing an active transport. Token bytes stay in the
//! runtime's tunnel thread.
use std::ptr;

use phux_client_runtime::tunnel::Tunnel;

use super::{PhuxRemoteTunnel, guard};
use crate::error::BridgeError;
use crate::types::PhuxClientResult;

impl PhuxRemoteTunnel {
    fn clone_resolved(&self) -> Result<Self, BridgeError> {
        let resolved = self.tunnel()?.resolved().clone();
        Ok(Self {
            inner: Ok(Tunnel::new(resolved)),
            name: self.name.clone(),
            endpoint: self.endpoint.clone(),
            session: self.session.clone(),
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
    use phux_client_runtime::tunnel::TunnelState;

    use super::*;

    #[test]
    fn clone_keeps_exact_endpoint_pin_and_token_provenance_after_registry_edit() {
        let dir = tempfile::tempdir().expect("fixture");
        let config = dir.path().join("config.toml");
        let token = dir.path().join("token-A");
        std::fs::write(&config, format!("[[remote]]\nname='mini'\nendpoint='wss://endpoint-A:8788'\ntoken-file='{}'\ncert-fingerprint='pin-A'\nsession='work'\n", token.display())).expect("config");
        let source = PhuxRemoteTunnel::resolve("mini", Some(&config));
        let source_tunnel = source.tunnel().expect("resolved");
        let original = source_tunnel.resolved().clone();
        assert_eq!(original.cert_fingerprint.as_deref(), Some("pin-A"));
        assert_eq!(original.token_file.as_deref(), Some(token.as_path()));
        std::fs::write(
            &config,
            "[[remote]]\nname='mini'\nendpoint='ws://endpoint-B:8788'\n",
        )
        .expect("rewrite");
        source_tunnel.inject_failure("old dial failed".into());
        assert_eq!(source_tunnel.state(), TunnelState::Failed);
        let mut cloned = ptr::null_mut();
        // SAFETY: live source and writable output; owns cloned handle below.
        assert_eq!(
            unsafe { phux_remote_tunnel_clone_resolved(&raw const source, &raw mut cloned) },
            PhuxClientResult::Ok
        );
        // SAFETY: successful clone returned a unique allocated handle.
        let cloned = unsafe { Box::from_raw(cloned) };
        let cloned_tunnel = cloned.tunnel().expect("cloned is resolved");
        assert_eq!(cloned_tunnel.resolved(), &original);
        // Fresh state, cancellation, and thread: the source's failure did not
        // propagate, and dropping the source (which cancels and joins its own
        // thread) leaves the clone RESOLVED and startable.
        assert_eq!(cloned_tunnel.state(), TunnelState::Resolved);
        drop(source);
        assert_eq!(cloned_tunnel.state(), TunnelState::Resolved);
        let (_embedder, tunnel_end) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        cloned_tunnel
            .start(tunnel_end)
            .expect("the clone starts after the source is gone");
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
