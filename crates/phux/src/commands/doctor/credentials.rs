//! Credential checks: the token store, workload authority, and client certificates.

use std::path::PathBuf;

use super::Check;

/// Can the credential store the remote listeners authenticate against be
/// read? A broken store admits no device while everything local stays green.
pub(super) fn check_token_store() -> Check {
    let path = std::env::var_os("PHUX_WS_TOKENS")
        .map_or_else(phux_server::auth::default_token_store_path, PathBuf::from);
    token_store_check(
        &path,
        phux_server::auth::ReloadingTokenStore::load(path.clone()).err(),
    )
}

/// The pure half of [`check_token_store`].
pub(super) fn token_store_check(
    path: &std::path::Path,
    error: Option<phux_server::auth::AuthError>,
) -> Check {
    let Some(error) = error else {
        return Check::pass(
            "token-store",
            format!("credential store at {} loads", path.display()),
        );
    };
    let remedy = if matches!(error, phux_server::auth::AuthError::LegacyStore) {
        format!(
            "a pre-versioned store is no longer read (ADR-0179): remove {} and pair \
             every device again with `phux pair`, then restart or `phux upgrade` a server \
             that disabled its listeners at boot",
            path.display()
        )
    } else {
        format!(
            "no device can authenticate until this loads; fix the file at {} (or point \
             PHUX_WS_TOKENS elsewhere), then restart or `phux upgrade` a server that \
             disabled its listeners at boot",
            path.display()
        )
    };
    Check::fail(
        "token-store",
        format!(
            "credential store at {} cannot be loaded ({error}) — no device credential \
             can be admitted, and a server that disabled its remote listeners at boot \
             stays down until this loads",
            path.display()
        ),
        remedy,
    )
}

pub(super) fn check_workload_authority() -> Check {
    let paths = phux_server::workload::WorkloadPaths::from_env();
    workload_authority_check(&paths.ca_cert, &paths.registry)
}

/// The pure half of [`check_workload_authority`]: the CA fingerprint and the
/// registry generation (ADR-0116). It never reads or names the CA key.
pub(super) fn workload_authority_check(
    ca_cert: &std::path::Path,
    registry: &std::path::Path,
) -> Check {
    use phux_server::workload::{WorkloadError, WorkloadRegistry, ca_fingerprint};
    let fingerprint = match ca_fingerprint(ca_cert) {
        Ok(fingerprint) => fingerprint,
        Err(WorkloadError::AuthorityMissing) => {
            return Check::pass(
                "workload-authority",
                "no workload CA; mTLS workload authority is not initialized",
            );
        }
        Err(error) => {
            return Check::fail(
                "workload-authority",
                format!(
                    "workload CA at {} cannot be read ({error})",
                    ca_cert.display()
                ),
                "a server in [policy] mode = \"paired\" refuses to start until the CA loads; fix the file's owner and mode, or remove the pair deliberately and re-enroll every client",
            );
        }
    };
    match WorkloadRegistry::load(registry) {
        Ok(snapshot) => {
            let now = chrono::Utc::now().timestamp();
            let active = snapshot
                .credentials()
                .iter()
                .filter(|credential| credential.is_active_at(now))
                .count();
            Check::pass(
                "workload-authority",
                format!(
                    "workload CA {fingerprint}; registry generation {}, {active} of {} credential(s) active",
                    snapshot.generation(),
                    snapshot.len()
                ),
            )
        }
        Err(error) => Check::fail(
            "workload-authority",
            format!(
                "workload CA {fingerprint}; registry at {} cannot be loaded ({error})",
                registry.display()
            ),
            "a running server admits no workload credential until the registry loads; restore it (owner-only, mode 0600) or re-enroll with `phux workload add-key`",
        ),
    }
}

/// Are the workload client certificates `phux host add` enrolled for this
/// machine's remotes good for a while yet (ADR-0116)? Reads only the public
/// certificates the `[[remote]]` entries name, never a key.
pub(super) fn check_client_certs() -> Check {
    match crate::commands::remote::load_registry() {
        Ok(entries) => client_certs_check(&entries, chrono::Utc::now().timestamp()),
        Err(err) => Check::warn(
            "client-certs",
            format!("could not read the remote registry ({err})"),
            "fix the config (`phux config check`), then rerun `phux doctor`",
        ),
    }
}

/// The pure half of [`check_client_certs`]: a remote naming half an
/// identity, or a certificate that is unreadable, expired, or within the
/// renewal window, warns with the command that fixes it.
pub(super) fn client_certs_check(
    entries: &[crate::commands::remote::RemoteEntry],
    now: i64,
) -> Check {
    use crate::commands::enroll::{admission_ends, calendar_date, renewal_due};
    let mut due = Vec::new();
    let mut soonest: Option<i64> = None;
    let mut enrolled = 0_usize;
    for entry in entries {
        let name = &entry.name;
        match (entry.client_cert.as_deref(), entry.client_key.as_deref()) {
            (None, None) => {}
            (Some(cert), Some(_)) => {
                enrolled += 1;
                if let Some(detail) = renewal_due(cert, now) {
                    due.push((
                        format!("{name}: {detail}"),
                        format!("phux host renew {name}"),
                    ));
                } else if let Some(ends) = admission_ends(cert) {
                    soonest = Some(soonest.map_or(ends, |soonest| soonest.min(ends)));
                }
            }
            _ => due.push((
                format!("{name}: names only one of client-cert and client-key"),
                format!("phux host add {}", entry.ssh_destination()),
            )),
        }
    }
    if !due.is_empty() {
        let (details, remedies): (Vec<_>, Vec<_>) = due.into_iter().unzip();
        return Check::warn(
            "client-certs",
            format!("workload client certificate: {}", details.join("; ")),
            format!(
                "run `{}`; a paired server refuses an expired certificate",
                remedies.join("`, `")
            ),
        );
    }
    match soonest {
        None if enrolled == 0 => Check::pass(
            "client-certs",
            "no remote presents an enrolled workload client certificate",
        ),
        None => Check::pass(
            "client-certs",
            format!("{enrolled} remote(s) present a workload client certificate"),
        ),
        Some(ends) => Check::pass(
            "client-certs",
            format!(
                "{enrolled} remote(s) present a workload client certificate; the first expires {}",
                calendar_date(ends)
            ),
        ),
    }
}

#[cfg(test)]
mod tests;
