//! Posture: which engine a server runs.

// --- Posture: which engine a server runs ---

/// The authorization posture a server starts in (`docs/remote.md#who-can-connect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyPosture {
    /// `mode = "local"`, or no mode and nothing past the owner's socket: the
    /// owner's socket only.
    Local,
    /// `mode = "paired"`: workload mTLS on every TLS transport, scoped
    /// grants at dispatch.
    Paired,
    /// No mode, and the embedder injected its own engine through
    /// `ServerConfig::policy_engine` (ADR-0072): that engine decides every
    /// HELLO, and listeners do not require a workload certificate. `phux
    /// server` never injects an engine, so no configuration reaches this.
    Embedded,
}

/// A `[policy]` setting the server refuses to start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PostureError {
    /// `PHUX_WORKLOAD_MTLS` is set. It was the opt-in before `[policy] mode`
    /// existed and is retired (ADR-0179); a server that still sees it says
    /// so rather than quietly choosing a mode for it.
    #[error(
        "PHUX_WORKLOAD_MTLS is retired: set [policy] mode = \"paired\" in config.toml and unset PHUX_WORKLOAD_MTLS"
    )]
    RetiredWorkloadMtlsEnv,
    /// `mode = "local"` beside a configured remote listener or connector.
    #[error(
        "[policy] mode = \"local\" admits the owner socket only, but a remote listener or relay connector is configured: remove it (--listen, --quic, --webtransport, PHUX_WS_ADDR, PHUX_QUIC_ADDR, PHUX_WT_ADDR, [[connector]]) or set mode = \"paired\""
    )]
    LocalWithRemoteListener,
    /// No mode beside a configured remote listener or connector: nothing
    /// would authorize the consumers it admits (ADR-0179 retired the owner
    /// grant they once got).
    #[error(
        "no [policy] mode is set, but a remote listener or relay connector is configured: set [policy] mode = \"paired\" in config.toml to serve enrolled clients, or remove it (--listen, --quic, --webtransport, PHUX_WS_ADDR, PHUX_QUIC_ADDR, PHUX_WT_ADDR, [[connector]])"
    )]
    UnsetWithRemoteListener,
    /// No mode beside a configured workload CA or registry location: the
    /// operator named workload authority but did not choose to enforce it.
    #[error(
        "no [policy] mode is set, but PHUX_WORKLOAD_CA, PHUX_WORKLOAD_CA_KEY, or PHUX_WORKLOAD_KEYS names a workload authority location: set [policy] mode = \"paired\" in config.toml to enforce it, or unset those variables"
    )]
    UnsetWithWorkloadAuthority,
}

/// What [`PolicyPosture::resolve`] decides from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent startup facts, each named"
)]
pub struct PostureInputs {
    /// `[policy] mode`.
    pub mode: Option<phux_config::PolicyMode>,
    /// Whether the retired `PHUX_WORKLOAD_MTLS` is set.
    pub retired_workload_mtls_env: bool,
    /// Whether a remote listener or relay connector is configured.
    pub remote_listener: bool,
    /// Whether a workload CA or registry location is configured
    /// (`PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, `PHUX_WORKLOAD_KEYS`).
    pub workload_authority_configured: bool,
    /// Whether the embedder injected its own engine (ADR-0072).
    pub embedder_engine: bool,
}

/// What [`PolicyPosture::resume_fallback`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeFallback {
    /// The posture the resumed server runs.
    pub posture: PolicyPosture,
    /// Why a cold start would have refused.
    pub refusal: PostureError,
    /// Whether every remote listener and relay connector stays unbound.
    pub remote_dropped: bool,
}

impl PolicyPosture {
    /// Decide the posture (`docs/remote.md#who-can-connect`). With no mode
    /// the server is `local`, and anything that would reach past the owner's
    /// socket is a startup error rather than a door nothing authorizes.
    ///
    /// # Errors
    ///
    /// A [`PostureError`] for the retired `PHUX_WORKLOAD_MTLS`, a `local`
    /// mode the rest of the configuration contradicts, or no mode beside a
    /// remote entry point or a configured workload authority location.
    pub const fn resolve(inputs: PostureInputs) -> Result<Self, PostureError> {
        if inputs.retired_workload_mtls_env {
            return Err(PostureError::RetiredWorkloadMtlsEnv);
        }
        match inputs.mode {
            Some(phux_config::PolicyMode::Paired) => Ok(Self::Paired),
            Some(phux_config::PolicyMode::Local) if inputs.remote_listener => {
                Err(PostureError::LocalWithRemoteListener)
            }
            None if inputs.embedder_engine => Ok(Self::Embedded),
            None if inputs.workload_authority_configured => {
                Err(PostureError::UnsetWithWorkloadAuthority)
            }
            None if inputs.remote_listener => Err(PostureError::UnsetWithRemoteListener),
            Some(phux_config::PolicyMode::Local) | None => Ok(Self::Local),
        }
    }

    /// The posture a resumed image runs when [`Self::resolve`] refused
    /// `inputs` with `refusal` (ADR-0032 graceful upgrade).
    ///
    /// Exiting there would end every pane the previous image handed over, so
    /// a resume fails closed instead of failing to start: the retired
    /// `PHUX_WORKLOAD_MTLS` is reported and ignored, and when the rest still
    /// refuses, the server runs `local` with every remote entry point left
    /// unbound. A cold start never takes this path.
    #[must_use]
    pub const fn resume_fallback(inputs: PostureInputs, refusal: PostureError) -> ResumeFallback {
        let without_env = PostureInputs {
            retired_workload_mtls_env: false,
            ..inputs
        };
        if let Ok(posture) = Self::resolve(without_env) {
            return ResumeFallback {
                posture,
                refusal,
                remote_dropped: false,
            };
        }
        ResumeFallback {
            posture: Self::Local,
            refusal,
            remote_dropped: inputs.remote_listener,
        }
    }

    /// Whether TLS listeners must require a workload client certificate.
    #[must_use]
    pub const fn requires_workload_mtls(self) -> bool {
        matches!(self, Self::Paired)
    }

    /// Whether the server admits the owner's socket only: no remote door,
    /// configured, auto-bound, or opened on demand.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local)
    }
}
