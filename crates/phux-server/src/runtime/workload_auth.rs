//! Atomic loading of explicitly requested workload authentication.
//!
//! Absence is the only way to choose bearer-only authentication. An error in
//! either half of configured workload authority must disable the listener.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::workload::{WorkloadError, WorkloadRegistry};

pub(super) struct WorkloadAuth {
    pub ca: PathBuf,
    pub registry: Arc<WorkloadRegistry>,
}

impl WorkloadAuth {
    /// `PHUX_WORKLOAD_MTLS` explicitly enables the profile, even on loopback.
    /// The other variables select material locations and do not enable it;
    /// provisioning those paths before opting in remains supported.
    pub(super) fn from_env() -> Result<Option<Self>, WorkloadError> {
        if std::env::var_os("PHUX_WORKLOAD_MTLS").is_none() {
            return Ok(None);
        }
        let cert = std::env::var_os("PHUX_WORKLOAD_CA")
            .map_or_else(crate::workload::default_ca_cert_path, PathBuf::from);
        let key = std::env::var_os("PHUX_WORKLOAD_CA_KEY")
            .map_or_else(crate::workload::default_ca_key_path, PathBuf::from);
        let registry = std::env::var_os("PHUX_WORKLOAD_KEYS")
            .map_or_else(crate::workload::default_registry_path, PathBuf::from);
        Self::load(&cert, &key, &registry).map(Some)
    }

    fn load(cert: &Path, key: &Path, registry: &Path) -> Result<Self, WorkloadError> {
        crate::workload::ensure_ca(cert, key)?;
        Ok(Self {
            ca: cert.to_owned(),
            registry: Arc::new(WorkloadRegistry::load(registry)?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_ca_pair_is_an_error_not_absent_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        std::fs::write(&cert, "existing cert").unwrap();
        assert!(matches!(
            WorkloadAuth::load(
                &cert,
                &dir.path().join("key.pem"),
                &dir.path().join("keys.json")
            ),
            Err(WorkloadError::PartialPair { .. })
        ));
    }

    #[test]
    fn malformed_registry_is_an_error_not_absent_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let registry = dir.path().join("keys.json");
        std::fs::write(&registry, "not json").unwrap();
        assert!(matches!(
            WorkloadAuth::load(
                &dir.path().join("ca.pem"),
                &dir.path().join("key.pem"),
                &registry
            ),
            Err(WorkloadError::Malformed(_))
        ));
    }

    #[test]
    fn missing_registry_retains_certificate_auth_with_no_enrolled_identities() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("ca.pem");
        let auth = WorkloadAuth::load(
            &cert,
            &dir.path().join("key.pem"),
            &dir.path().join("keys.json"),
        )
        .unwrap();
        assert_eq!(auth.ca, cert);
        assert!(auth.registry.is_empty());
    }

    /// Use subprocess-local environment overrides: the production factory reads
    /// environment configuration and Rust tests otherwise share that process.
    #[test]
    fn configured_workload_failure_disables_quic() {
        if std::env::var_os("PHUX_TEST_WORKLOAD_FAILURE").is_some() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _entered = runtime.enter();
            let (listener, slot) =
                super::super::build_quic_listener("127.0.0.1:0".parse().unwrap());
            assert!(
                listener.is_none(),
                "failed explicit mTLS must never fall back to bearer or anonymous QUIC"
            );
            assert!(slot.is_unhealthy());
            assert_eq!(
                slot.disabled_reason,
                Some(super::super::ListenerDisabledReason::TlsSetupFailed)
            );
            return;
        }
        for (malformed_registry, secure) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let dir = tempfile::tempdir().unwrap();
            let leaf = dir.path().join("leaf.pem");
            let leaf_key = dir.path().join("leaf-key.pem");
            crate::transport::tls::ensure_self_signed_for(
                &leaf,
                &leaf_key,
                &["localhost".to_owned()],
            )
            .unwrap();
            let ca = dir.path().join("ca.pem");
            let registry = dir.path().join("keys.json");
            if malformed_registry {
                std::fs::write(&registry, "malformed").unwrap();
            } else {
                std::fs::write(&ca, "partial CA pair").unwrap();
            }
            let tokens = dir.path().join("tokens.json");
            crate::auth::write_test_credential(&tokens, &[0x11; crate::auth::TOKEN_LEN]);
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "runtime::workload_auth::tests::configured_workload_failure_disables_quic",
                    "--nocapture",
                ])
                .env("PHUX_TEST_WORKLOAD_FAILURE", "1")
                .env("PHUX_WORKLOAD_MTLS", "1")
                .env("PHUX_WORKLOAD_CA", ca)
                .env("PHUX_WORKLOAD_CA_KEY", dir.path().join("ca-key.pem"))
                .env("PHUX_WORKLOAD_KEYS", registry)
                .env("PHUX_WS_TLS_CERT", leaf)
                .env("PHUX_WS_TLS_KEY", leaf_key)
                .env("PHUX_WS_TOKENS", tokens)
                .env("PHUX_WS_SECURE", if secure { "1" } else { "" })
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
