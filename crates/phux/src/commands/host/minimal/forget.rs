//! Forgetting a box: its satellite entry goes and its credentials are revoked.

use std::collections::BTreeSet;

use super::super::super::json_err::{CliError, codes};
use super::super::super::satellite as satellite_registry;
use super::super::hub::reload_local_hub;
use super::super::inbound;

/// Remove `satellite`'s `inbound://` entry and revoke its credentials.
///
/// Revocation is always attempted for a name this feature minted, even when
/// the satellite registry cannot be read. Any failure is returned so the
/// caller keeps the box and retries; this function never reports the
/// satellite revoked. A name `minted` does not contain is left untouched.
pub(super) fn forget_box(
    satellite: &str,
    minted: &BTreeSet<String>,
    files: &inbound::HubFiles,
) -> Result<(), CliError> {
    if !minted.contains(satellite) {
        return Err(CliError::new(
            codes::REGISTRY,
            format!("satellite {satellite} was not minted by `phux host minimal`; leaving it"),
            "remove that satellite with `phux host rm` if it should go",
        ));
    }
    let loaded = satellite_registry::load_registry_from(&files.config);
    let revoked = inbound::revoke_satellite_at(&files.workload.registry, satellite);
    let removed = loaded.as_ref().map_or(Ok(()), |entries| {
        entries
            .iter()
            .find(|entry| {
                entry.name == satellite && phux_server::hub::is_inbound_endpoint(&entry.endpoint)
            })
            .map_or(Ok(()), |entry| {
                satellite_registry::remove_entry_at(&files.config, entry)
            })
    });
    let _ = reload_local_hub();
    let mut problems = Vec::new();
    if let Err(err) = &loaded {
        problems.push(format!("could not read the satellite registry ({err})"));
    }
    if let Err(err) = removed {
        problems.push(format!("could not remove its satellite entry ({err})"));
    }
    if let Err(err) = revoked {
        problems.push(format!("could not revoke its credential ({err})"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CliError::new(
            codes::WORKLOAD,
            format!("could not forget {satellite}: {}", problems.join("; ")),
            "the box stays recorded so the next sync can retry",
        ))
    }
}
