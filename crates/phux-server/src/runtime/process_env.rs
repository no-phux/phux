//! The server's `PHUX_*` process configuration, resolved once.
//!
//! The daemon's remote surface (listener addresses, TLS pair, credential
//! store, workload mode) is configurable through environment variables, and
//! a running server exports several of them into every pane. Reading them
//! ad hoc from deep inside the runtime made every in-process server (tests,
//! embedders) inherit whatever the launching shell carried: run from a
//! production pane, a test server picked up the operator's certificate and
//! credential store. So the binary snapshots them here once, at startup
//! ([`ServerEnv::from_process`]), and the runtime reads only this snapshot.
//! [`ServerEnv::default`] sets nothing, which makes an in-process server
//! hermetic unless its builder opts a variable in.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use tracing::warn;

/// `PHUX_*` process configuration the server runtime consults. Every field
/// is `None`/`false` by default (nothing set).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerEnv {
    /// `PHUX_WS_ADDR`: WebSocket listen address when no flag gives one.
    pub ws_addr: Option<SocketAddr>,
    /// `PHUX_QUIC_ADDR`: QUIC listen address when no flag gives one.
    pub quic_addr: Option<SocketAddr>,
    /// `PHUX_WT_ADDR`: WebTransport listen address when no flag gives one.
    pub wt_addr: Option<SocketAddr>,
    /// `PHUX_WS_SECURE` (non-empty): force TLS and token auth on loopback.
    pub ws_secure: bool,
    /// `PHUX_WS_ALLOWED_ORIGINS`: browser origins the plaintext loopback
    /// WebSocket listener admits beyond loopback ones (comma-separated, or
    /// `*`).
    pub ws_allowed_origins: Option<String>,
    /// `PHUX_WS_TOKENS`: the bearer-token store, instead of the default.
    pub ws_tokens: Option<PathBuf>,
    /// `PHUX_WS_TLS_CERT`: the operator's certificate, instead of the
    /// shared self-signed one.
    pub tls_cert: Option<PathBuf>,
    /// `PHUX_WS_TLS_KEY`: the operator's private key (see [`Self::tls_cert`]).
    pub tls_key: Option<PathBuf>,
    /// `PHUX_WORKLOAD_MTLS` (set): request workload mode (ADR-0116).
    pub workload_mtls: bool,
    /// `PHUX_WORKLOAD_CA`: workload authority certificate location.
    pub workload_ca: Option<PathBuf>,
    /// `PHUX_WORKLOAD_CA_KEY`: workload authority key location.
    pub workload_ca_key: Option<PathBuf>,
    /// `PHUX_WORKLOAD_KEYS`: workload credential registry location.
    pub workload_keys: Option<PathBuf>,
    /// `PHUX_NO_AUTO_LISTEN` (set): never auto-bind an overlay listener.
    pub no_auto_listen: bool,
    /// `PHUX_UPLOAD_DIR` (non-empty): where `PUT_FILE` lands uploads.
    pub upload_dir: Option<PathBuf>,
    /// `PHUX_UPLOAD_MAX_BYTES`: total bytes the upload directory may hold
    /// (`0` = unlimited); unset or malformed (warned) is the default.
    pub upload_max_bytes: Option<u64>,
    /// `PHUX_UPLOAD_MAX_FILES`: uploads the upload directory may hold
    /// (`0` = unlimited); unset or malformed (warned) is the default.
    pub upload_max_files: Option<u64>,
    /// `PHUX_SSH`: the program hub links dial SSH satellites with.
    pub ssh_program: Option<OsString>,
}

impl ServerEnv {
    /// Snapshot the variables from this process's environment. Only
    /// process-entry binaries (`phux server`, `ws_demo_server`) should call
    /// this; in-process embedders and tests build a [`ServerEnv`] explicitly.
    #[must_use]
    pub fn from_process() -> Self {
        Self::from_lookup(|var| std::env::var_os(var))
    }

    /// Resolve the variables through `lookup` (a variable name to its value).
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>) -> Self {
        let path = |var: &str| lookup(var).map(PathBuf::from);
        let non_empty = |var: &str| lookup(var).filter(|value| !value.is_empty());
        Self {
            ws_addr: socket_addr(&lookup, "PHUX_WS_ADDR"),
            quic_addr: socket_addr(&lookup, "PHUX_QUIC_ADDR"),
            wt_addr: socket_addr(&lookup, "PHUX_WT_ADDR"),
            ws_secure: non_empty("PHUX_WS_SECURE").is_some(),
            ws_allowed_origins: non_empty("PHUX_WS_ALLOWED_ORIGINS")
                .map(|value| value.to_string_lossy().into_owned()),
            ws_tokens: path("PHUX_WS_TOKENS"),
            tls_cert: path("PHUX_WS_TLS_CERT"),
            tls_key: path("PHUX_WS_TLS_KEY"),
            workload_mtls: lookup("PHUX_WORKLOAD_MTLS").is_some(),
            workload_ca: path("PHUX_WORKLOAD_CA"),
            workload_ca_key: path("PHUX_WORKLOAD_CA_KEY"),
            workload_keys: path("PHUX_WORKLOAD_KEYS"),
            no_auto_listen: lookup(super::DISABLE_AUTO_LISTEN_ENV).is_some(),
            upload_dir: non_empty("PHUX_UPLOAD_DIR").map(PathBuf::from),
            upload_max_bytes: count(&lookup, "PHUX_UPLOAD_MAX_BYTES"),
            upload_max_files: count(&lookup, "PHUX_UPLOAD_MAX_FILES"),
            ssh_program: lookup("PHUX_SSH"),
        }
    }

    /// The bearer-token store path: [`Self::ws_tokens`], else the default.
    #[must_use]
    pub fn tokens_path(&self) -> PathBuf {
        self.ws_tokens
            .clone()
            .unwrap_or_else(crate::auth::default_token_store_path)
    }

    /// The workload authority locations: each override, else its default.
    #[must_use]
    pub fn workload_paths(&self) -> crate::workload::WorkloadPaths {
        crate::workload::WorkloadPaths::with_overrides(
            self.workload_ca.clone(),
            self.workload_ca_key.clone(),
            self.workload_keys.clone(),
        )
    }

    /// The SSH program hub links dial with: [`Self::ssh_program`], else
    /// `ssh`.
    #[must_use]
    pub fn ssh_program(&self) -> OsString {
        self.ssh_program.clone().unwrap_or_else(|| "ssh".into())
    }
}

/// Parse a non-negative integer from variable `var`; unset or empty is
/// `None`, and malformed is `None` with a warning (the default applies).
fn count(lookup: &impl Fn(&str) -> Option<OsString>, var: &str) -> Option<u64> {
    let raw = lookup(var).filter(|value| !value.is_empty())?;
    let raw = raw.to_string_lossy();
    match raw.trim().parse::<u64>() {
        Ok(value) => Some(value),
        Err(err) => {
            warn!(var, value = %raw, error = %err, "invalid count; using the default");
            None
        }
    }
}

/// Parse a [`SocketAddr`] from variable `var`; unset or malformed (warned)
/// leaves the transport disabled.
fn socket_addr(lookup: &impl Fn(&str) -> Option<OsString>, var: &str) -> Option<SocketAddr> {
    let raw = lookup(var)?;
    let raw = raw
        .to_str()
        .map_or_else(|| raw.to_string_lossy().into_owned(), str::to_owned);
    match raw.parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(err) => {
            warn!(var, addr = %raw, error = %err, "invalid socket address; transport disabled");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_hermetic() {
        assert_eq!(ServerEnv::from_lookup(|_| None), ServerEnv::default());
    }

    #[test]
    fn lookup_resolves_every_variable() {
        let env = ServerEnv::from_lookup(|var| {
            Some(match var {
                "PHUX_WS_ADDR" => "127.0.0.1:1".into(),
                "PHUX_QUIC_ADDR" => "not an address".into(),
                "PHUX_WS_SECURE" | "PHUX_UPLOAD_DIR" => OsString::new(),
                "PHUX_UPLOAD_MAX_BYTES" => "1024".into(),
                "PHUX_UPLOAD_MAX_FILES" => "lots".into(),
                _ => format!("/x/{var}").into(),
            })
        });
        assert_eq!(env.ws_addr, Some("127.0.0.1:1".parse().unwrap()));
        assert_eq!(env.quic_addr, None, "malformed is disabled, not fatal");
        assert!(!env.ws_secure, "an empty PHUX_WS_SECURE does not force TLS");
        assert_eq!(env.upload_dir, None, "an empty PHUX_UPLOAD_DIR is unset");
        assert_eq!(env.upload_max_bytes, Some(1024));
        assert_eq!(env.upload_max_files, None, "malformed keeps the default");
        assert_eq!(env.tls_cert, Some(PathBuf::from("/x/PHUX_WS_TLS_CERT")));
        assert_eq!(
            env.ws_allowed_origins.as_deref(),
            Some("/x/PHUX_WS_ALLOWED_ORIGINS")
        );
        assert_eq!(env.tokens_path(), PathBuf::from("/x/PHUX_WS_TOKENS"));
        assert!(env.workload_mtls && env.no_auto_listen);
        assert_eq!(env.ssh_program(), OsString::from("/x/PHUX_SSH"));
    }
}
