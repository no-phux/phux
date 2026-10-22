//! The server's TLS certificate: overlay coverage and its chain to the workload CA.

use std::path::PathBuf;

use super::Check;

pub(super) fn check_remote_cert() -> Check {
    let operator_cert = std::env::var_os("PHUX_WS_TLS_CERT").is_some()
        || std::env::var_os("PHUX_WS_TLS_KEY").is_some();
    let cert = std::env::var_os("PHUX_WS_TLS_CERT").map_or_else(
        phux_server::transport::tls::default_cert_path,
        PathBuf::from,
    );
    let key = std::env::var_os("PHUX_WS_TLS_KEY")
        .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from);
    // Same overlay detection `phux pair` uses (ADR-0037).
    let advertised: Vec<String> = phux_config::overlay::detect()
        .into_iter()
        .map(phux_server::transport::tls::san_name)
        .collect();
    remote_cert_check(&cert, &key, &advertised, operator_cert)
}

/// Does the remote certificate name the advertised overlay address? SANs are
/// fixed at generation, and regenerating rotates the pinned fingerprint
/// (ADR-0091). Warn, never fail: phux consumers pin the fingerprint.
pub(super) fn remote_cert_check(
    cert: &std::path::Path,
    key: &std::path::Path,
    advertised: &[String],
    operator_cert: bool,
) -> Check {
    let source = if operator_cert {
        "operator-supplied"
    } else {
        "auto-provisioned"
    };
    if !cert.exists() {
        return Check::warn(
            "remote-cert",
            format!("no {source} certificate at {}", cert.display()),
            "run `phux pair` — it provisions the certificate and mints a device token",
        );
    }
    if advertised.is_empty() {
        return Check::warn(
            "remote-cert",
            format!(
                "{source} certificate at {}; no overlay address detected, so its \
                 coverage of a routable address cannot be checked",
                cert.display()
            ),
            "nothing to do unless you expected an overlay: check PHUX_OVERLAY_ADDRS or `tailscale ip -4`",
        );
    }
    match phux_server::transport::tls::uncovered_names(cert, advertised) {
        Err(err) => Check::fail(
            "remote-cert",
            format!("cannot read {}: {err}", cert.display()),
            "the remote listener will not start; remove the unreadable file and \
             re-run `phux pair`",
        ),
        Ok(uncovered) if uncovered.is_empty() => Check::pass(
            "remote-cert",
            format!(
                "{source} certificate at {} names {}",
                cert.display(),
                advertised.join(", ")
            ),
        ),
        Ok(uncovered) => Check::warn(
            "remote-cert",
            format!(
                "{source} certificate at {} does not name {} — fingerprint-pinning \
                 devices are unaffected, but a client that validates the server name \
                 (a browser, or curl --cacert) will refuse the handshake",
                cert.display(),
                uncovered.join(", ")
            ),
            if operator_cert {
                "reissue the certificate with those addresses in its subjectAltName".to_owned()
            } else {
                format!(
                    "regenerating is the only fix and it rotates the pinned fingerprint, \
                     un-pairing every paired device: `rm {} {} && phux pair`, then re-pair \
                     each device",
                    cert.display(),
                    key.display()
                )
            },
        ),
    }
}

/// Diagnose auto-provisioned server identity without touching the certificate,
/// key, or CA. Operator TLS has its own trust policy and is not enrolled by
/// the workload CA.
pub(super) fn check_remote_cert_chain() -> Option<Check> {
    let operator_cert = std::env::var_os("PHUX_WS_TLS_CERT").is_some()
        || std::env::var_os("PHUX_WS_TLS_KEY").is_some();
    let ca = phux_server::workload::WorkloadPaths::from_env().ca_cert;
    remote_cert_chain_check(
        &phux_server::transport::tls::default_cert_path(),
        &ca,
        operator_cert,
    )
}

pub(super) fn remote_cert_chain_check(
    cert: &std::path::Path,
    ca: &std::path::Path,
    operator_cert: bool,
) -> Option<Check> {
    // Missing material is reported by existing checks. Operator TLS uses its
    // own trust policy, even if it happens to be signed by the workload CA.
    if operator_cert || !cert.exists() || !ca.exists() {
        return None;
    }
    let name = "remote-cert-chain";
    let authority = match phux_server::workload::ca_fingerprint(ca) {
        Ok(authority) => authority,
        Err(error) => {
            return Some(Check::warn(
                name,
                format!(
                    "cannot verify server certificate against workload CA at {}: {error}",
                    ca.display()
                ),
                "inspect the workload-authority check; no certificate was changed",
            ));
        }
    };
    match phux_server::transport::tls::presented_authority(cert) {
        Ok(Some(presented)) if presented == authority => Some(Check::pass(
            name,
            "auto-provisioned server certificate chains to the workload CA",
        )),
        Ok(_) => Some(Check::warn(
            name,
            format!(
                "auto-provisioned server certificate at {} does not chain to the workload CA",
                cert.display()
            ),
            "phone QR enrollment is blocked: phones pin the workload CA, not this legacy leaf. \
             Do not rotate the CA as a quick fix: rotation invalidates every paired client and \
             workload credential; arrange a deliberate same-CA server-leaf reissue with pin \
             migration instead",
        )),
        Err(error) => Some(Check::warn(
            name,
            format!(
                "cannot verify auto-provisioned server certificate at {}: {error}",
                cert.display()
            ),
            "inspect the remote-cert check; no certificate was changed",
        )),
    }
}

#[cfg(test)]
mod tests;
