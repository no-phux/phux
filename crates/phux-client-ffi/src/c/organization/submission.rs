//! Liveness and capacity checks, operation submission, and key decoding.

use super::{
    BridgeError, Client, ControlPlane, Kind, OperationId, Part, Request, Resolution, ResourceKey,
    SessionKey, SubmitError,
};

// ----- submission -------------------------------------------------------------------

fn ensure_live(client: &Client) -> Result<(), BridgeError> {
    if !client.protocol_ready || client.detached {
        return Err(BridgeError::state(
            "organization operations need a negotiated connection",
        ));
    }
    if !client.control().organization_supported() {
        return Err(BridgeError::state(
            "the server does not advertise ORGANIZATION",
        ));
    }
    if !client.control().organization().is_live() {
        return Err(BridgeError::state(
            "the organization records are not loaded yet; wait for PhuxOrganizationInfo.state LIVE",
        ));
    }
    Ok(())
}

/// Whether a typed command submitted now would ever reach an `ORGANIZATION`
/// server. The runtime's journal holds it until a seed names the authority
/// and then sends it, so a seed in flight (or an authority already known,
/// even while a journal gap reseeds) is enough. Only a connection whose seed
/// was refused or never sent, with no authority, would hold it forever: that
/// is refused until `phux_client_organization_refresh`.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the control guard lives exactly as long as the replica borrow it lends"
)]
pub(crate) fn ensure_deliverable(client: &Client) -> Result<(), BridgeError> {
    let control = client.control();
    if !control.organization_supported() {
        return Ok(());
    }
    let replica = control.organization();
    if replica.authority().is_none() && !replica.seeding() {
        return Err(BridgeError::state(
            "the organization records could not be read; call phux_client_organization_refresh",
        ));
    }
    Ok(())
}

fn ensure_capacity(client: &Client, request_id: u32) -> Result<(), BridgeError> {
    client.operations.check_request_id(request_id)?;
    if client.org.len() >= crate::c::operations::MAX_OPERATIONS {
        return Err(BridgeError::state(
            "organization results are full; release completed results",
        ));
    }
    Ok(())
}

pub(crate) fn submit_error(error: &SubmitError) -> BridgeError {
    match error {
        SubmitError::NotAnOperation => {
            BridgeError::state("the organization replica does not hold that record")
        }
        other => BridgeError::state(other.to_string()),
    }
}

/// Validate, submit through `submit`, and retain the request.
pub(super) fn submit(
    client: &mut Client,
    request_id: u32,
    kind: Kind,
    keys: Vec<Vec<[u8; 16]>>,
    is_session: bool,
    submit: impl FnOnce(&mut ControlPlane) -> Result<Vec<OperationId>, SubmitError>,
) -> Result<(), BridgeError> {
    ensure_live(client)?;
    ensure_capacity(client, request_id)?;
    let operations = {
        let mut control = client.control();
        submit(&mut control).map_err(|error| submit_error(&error))?
    };
    client.operations.consume_request_id(request_id);
    let mut request = Request::new(kind);
    request.parts = operations
        .into_iter()
        .zip(keys)
        .map(|(operation, keys)| Part {
            operation,
            keys,
            is_session,
            resolution: Resolution::pending(),
            receipt: None,
        })
        .collect();
    request.refresh_view();
    client.org.requests.insert(request_id, request);
    client.process_runtime_events()?;
    Ok(())
}

unsafe fn key_in(key: *const u8) -> Result<[u8; 16], BridgeError> {
    if key.is_null() {
        return Err(BridgeError::invalid("key is null"));
    }
    // SAFETY: the caller promises 16 readable bytes at a non-null `key`.
    let bytes = unsafe { std::slice::from_raw_parts(key, 16) };
    let mut out = [0; 16];
    out.copy_from_slice(bytes);
    Ok(out)
}

pub(super) unsafe fn session_key_in(key: *const u8) -> Result<SessionKey, BridgeError> {
    // SAFETY: forwards the caller's 16-byte contract.
    let bytes = unsafe { key_in(key) }?;
    SessionKey::new(bytes).ok_or_else(|| BridgeError::invalid("session key is all zero"))
}

pub(super) unsafe fn resource_key_in(key: *const u8) -> Result<ResourceKey, BridgeError> {
    // SAFETY: forwards the caller's 16-byte contract.
    let bytes = unsafe { key_in(key) }?;
    ResourceKey::new(bytes).ok_or_else(|| BridgeError::invalid("resource key is all zero"))
}
