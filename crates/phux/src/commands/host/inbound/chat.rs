//! Native conversation enrollment, without adopting terminal satellites.

use super::{
    CliError, HubFiles, JoinCode, WorkloadPaths, inbound_entry_enabled, pinned_authority,
    registry_failure, reject, require_paired, tickets, withdraw_ticket,
};

/// An invitation for an explicitly scoped conversation, never a terminal link.
pub(crate) struct MailInvitation {
    pub(crate) code: JoinCode,
    pub(crate) ticket_id: String,
    pub(crate) expires_at: i64,
}

/// Mint through the same enrollment store without registering a terminal satellite.
pub(crate) fn mint_mail_invitation(
    name: &str,
    hub: &str,
    minutes: i64,
) -> Result<MailInvitation, CliError> {
    let files = HubFiles::from_env();
    require_paired(&files.config)?;
    phux_server::workload::validate_satellite_name(name).map_err(|err| reject(err.to_string()))?;
    phux_work::Address::parse(&format!("{name}/inbox")).map_err(|err| reject(err.to_string()))?;
    phux_server::uplink::validate_hub_endpoint(hub).map_err(reject)?;
    // A conversation never adopts a terminal satellite or rings a config
    // reload doorbell: enrollment reads the existing ticket store directly.
    inbound_entry_enabled(&files.config, name, false)?;
    let ca = pinned_authority(&files.cert)?;
    let ticket = tickets::mint_satellite_ticket(
        &tickets::tickets_path(&files.workload.registry),
        name,
        tickets::DEFAULT_CREDENTIAL_SECONDS,
        minutes.saturating_mul(60),
    )
    .map_err(|err| registry_failure(err.to_string()))?;
    Ok(MailInvitation {
        code: JoinCode {
            hub: hub.to_owned(),
            server_name: None,
            ca,
            name: name.to_owned(),
            ticket: ticket.secret_hex,
        },
        ticket_id: ticket.id,
        expires_at: ticket.expires_at,
    })
}

/// Withdraw a conversation invitation that could not be published.
pub(crate) fn withdraw_mail_invitation(id: &str) -> Result<(), String> {
    withdraw_ticket(&WorkloadPaths::from_env().registry, id)
}
