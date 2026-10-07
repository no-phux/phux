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

use crate::workload::{ReloadingWorkloadRegistry, WorkloadError};

pub(super) struct WorkloadAuth {
    /// The workload CA certificate, read once through the workload store's
    /// owner, mode, and no-follow checks. The TLS verifiers are built from
    /// these bytes; the path is never re-read.
    pub ca: CertificateDer<'static>,
    pub registry: Arc<ReloadingWorkloadRegistry>,
}

impl WorkloadAuth {
    /// The authority a listener needs under its posture: `None` unless the
    /// `paired` posture requires workload mTLS (`[policy] mode = "paired"`,
    /// or `PHUX_WORKLOAD_MTLS` with no mode). It applies even on loopback.
    pub(super) fn for_posture(
        required: bool,
        env: &super::ServerEnv,
    ) -> Result<Option<Self>, WorkloadError> {
        if !required {
            return Ok(None);
        }
        Self::configured(env).map(Some)
    }

    /// Load the authority from its configured locations. The
    /// `PHUX_WORKLOAD_*` path variables select locations and do not enable
    /// anything; provisioning those paths before opting in remains
    /// supported.
    pub(super) fn configured(env: &super::ServerEnv) -> Result<Self, WorkloadError> {
        let paths = env.workload_paths();
        Self::load(&paths.ca_cert, &paths.ca_key, &paths.registry)
    }

    fn load(cert: &Path, key: &Path, registry: &Path) -> Result<Self, WorkloadError> {
        crate::workload::ensure_ca(cert, key)?;
        Ok(Self {
            ca: crate::workload::authority_certificate(cert)?,
            registry: Arc::new(ReloadingWorkloadRegistry::load(registry.to_owned())?),
        })
    }
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
            remedy: "remove `--webtransport` and PHUX_WT_ADDR, or leave workload mode (unset PHUX_WORKLOAD_MTLS and [policy] mode = \"paired\")",
        });
    }
    if relay_connectors {
        return Err(super::ServerError::WorkloadModeUncovered {
            surface: "a relay connector (`[[connector]]` in config.toml)",
            remedy: "remove the `[[connector]]` entries, or leave workload mode (unset PHUX_WORKLOAD_MTLS and [policy] mode = \"paired\")",
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

    /// A configured authority that cannot load disables the QUIC listener,
    /// never falling back to bearer or anonymous admission.
    #[test]
    fn configured_workload_failure_disables_quic() {
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
            let env = super::super::ServerEnv {
                workload_mtls: true,
                workload_ca: Some(ca),
                workload_ca_key: Some(dir.path().join("ca-key.pem")),
                workload_keys: Some(registry),
                tls_cert: Some(leaf),
                tls_key: Some(leaf_key),
                ws_tokens: Some(tokens),
                ws_secure: secure,
                ..super::super::ServerEnv::default()
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _entered = runtime.enter();
            let (listener, slot) = super::super::build_quic_listener_for(
                "127.0.0.1:0".parse().unwrap(),
                env.workload_mtls,
                &env,
            );
            assert!(
                listener.is_none(),
                "failed explicit mTLS must never fall back to bearer or anonymous QUIC"
            );
            assert!(slot.is_unhealthy());
            assert_eq!(
                slot.disabled_reason,
                Some(super::super::ListenerDisabledReason::TlsSetupFailed)
            );
        }
    }
}
