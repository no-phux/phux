//! Atomic loading of explicitly requested workload authentication.
//!
//! Absence is the only way to choose bearer-only authentication. An error in
//! either half of configured workload authority must disable the listener.

//!
//! The registry is loaded strictly once, at listener build, and then tracked:
//! every accepted connection consults its current generation, so
//! `phux workload add-key` and `revoke` apply without a restart.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::CertificateDer;

use crate::workload::{ReloadingWorkloadRegistry, WorkloadError, WorkloadPaths};

pub(super) struct WorkloadAuth {
    /// The workload CA certificate, read once through the workload store's
    /// owner, mode, and no-follow checks. The TLS verifiers are built from
    /// these bytes; the path is never re-read.
    pub ca: CertificateDer<'static>,
    pub registry: Arc<ReloadingWorkloadRegistry>,
}

impl WorkloadAuth {
    /// `PHUX_WORKLOAD_MTLS` explicitly enables the profile, even on loopback.
    /// The other variables select material locations and do not enable it;
    /// provisioning those paths before opting in remains supported.
    pub(super) fn from_env() -> Result<Option<Self>, WorkloadError> {
        if std::env::var_os("PHUX_WORKLOAD_MTLS").is_none() {
            return Ok(None);
        }
        let paths = WorkloadPaths::from_env();
        Self::load(&paths.ca_cert, &paths.ca_key, &paths.registry).map(Some)
    }

    fn load(cert: &Path, key: &Path, registry: &Path) -> Result<Self, WorkloadError> {
        crate::workload::ensure_ca(cert, key)?;
        Ok(Self {
            ca: crate::workload::authority_certificate(cert)?,
            registry: Arc::new(ReloadingWorkloadRegistry::load(registry.to_owned())?),
        })
    }
}

/// Whether workload mode is requested (`PHUX_WORKLOAD_MTLS`).
pub(super) fn workload_mode() -> bool {
    std::env::var_os("PHUX_WORKLOAD_MTLS").is_some()
}

/// Refuse to start workload mode beside a remote entry point that cannot
/// require a workload client certificate: the WebTransport listener (a
/// browser session presents no client certificate) and a relay connector
/// (the relay terminates TLS, so a consumer's certificate never reaches this
/// server). Fail closed with the remedy named rather than leave either door
/// on bearer-only admission (ADR-0116).
pub(super) const fn refuse_uncovered_surfaces(
    workload_mode: bool,
    relay_connectors: bool,
    webtransport: bool,
) -> Result<(), super::ServerError> {
    if !workload_mode {
        return Ok(());
    }
    if webtransport {
        return Err(super::ServerError::WorkloadModeUncovered {
            surface: "the WebTransport listener",
            remedy: "remove `--webtransport` and PHUX_WT_ADDR, or unset PHUX_WORKLOAD_MTLS",
        });
    }
    if relay_connectors {
        return Err(super::ServerError::WorkloadModeUncovered {
            surface: "a relay connector (`[[connector]]` in config.toml)",
            remedy: "remove the `[[connector]]` entries, or unset PHUX_WORKLOAD_MTLS",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workload_mode_refuses_entry_points_that_cannot_carry_a_certificate() {
        assert!(
            refuse_uncovered_surfaces(false, true, true).is_ok(),
            "without workload mode nothing is refused"
        );
        assert!(refuse_uncovered_surfaces(true, false, false).is_ok());
        for (connectors, webtransport, surface) in [
            (false, true, "WebTransport"),
            (true, false, "[[connector]]"),
            (true, true, "WebTransport"),
        ] {
            let message = refuse_uncovered_surfaces(true, connectors, webtransport)
                .unwrap_err()
                .to_string();
            assert!(
                message.contains("PHUX_WORKLOAD_MTLS")
                    && message.contains(surface)
                    && message.contains("unset PHUX_WORKLOAD_MTLS"),
                "the refusal names the setting, the entry point, and the remedy: {message}"
            );
        }
    }

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
        // Owner-only, so the refusal is about the content, not the mode.
        std::fs::set_permissions(
            &registry,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .unwrap();
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
        assert_eq!(
            auth.ca,
            crate::workload::authority_certificate(&cert).unwrap()
        );
        assert!(auth.registry.current().is_empty());
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
