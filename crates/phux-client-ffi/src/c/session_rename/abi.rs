//! The session-rename C ABI: start a rename, follow names, read the outcome.

use super::{
    BridgeError, PhuxBytes, PhuxClient, PhuxClientResult, RenamePlan, STATUS_PENDING,
    STATUS_REFUSED, STATUS_RENAMED, bytes_in, bytes_out, check_struct, mem, plan, queue_rename,
    submit_typed, subscribe, valid_name, with_client_mut, with_client_ref,
};

/// Rename the session named `current` to `new_name` on the connected server.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; each span is
/// readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_rename_session(
    client: *mut PhuxClient,
    request_id: u32,
    current: PhuxBytes,
    new_name: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached {
            return Err(BridgeError::state("a rename needs a negotiated connection"));
        }
        if client.session_rename.pending() {
            return Err(BridgeError::state("a rename is already pending"));
        }
        // SAFETY: caller supplies readable spans.
        let current = unsafe { bytes_in(current.data, current.len) }?;
        // SAFETY: as above.
        let new_name = unsafe { bytes_in(new_name.data, new_name.len) }?;
        valid_name(current)?;
        valid_name(new_name)?;
        let current_name = std::str::from_utf8(current)
            .map_err(|_| BridgeError::invalid("a session name must be UTF-8"))?;
        let new_name_str = std::str::from_utf8(new_name)
            .map_err(|_| BridgeError::invalid("a session name must be UTF-8"))?;
        client.operations.check_request_id(request_id)?;
        if client.outgoing.len() + 3 > crate::c::operations::MAX_OPERATIONS {
            return Err(BridgeError::state(
                "outgoing queue is full; drain outgoing frames before renaming",
            ));
        }
        let judged = plan(client, current_name, new_name_str);
        if matches!(judged, RenamePlan::Send { .. }) {
            crate::c::organization::ensure_deliverable(client)?;
        }
        client.operations.consume_request_id(request_id);
        let rename = &mut client.session_rename;
        rename.request_id = request_id;
        rename.current = current.to_vec();
        rename.new_name = new_name.to_vec();
        match judged {
            RenamePlan::Refused(reason) => {
                rename.session_id = 0;
                rename.settle(STATUS_REFUSED, &reason.to_string());
            }
            RenamePlan::Unchanged { session_id } => {
                rename.session_id = session_id.get();
                rename.settle(STATUS_RENAMED, "");
            }
            RenamePlan::Send { session_id } => {
                rename.session_id = session_id.get();
                rename.settle(STATUS_PENDING, "");
                match submit_typed(client, session_id.get(), new_name_str) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        client.session_rename.settle(STATUS_REFUSED, &error.message);
                        return Err(error);
                    }
                }
                if let Err(error) = queue_rename(client, request_id, current_name, new_name_str) {
                    client.session_rename.settle(STATUS_REFUSED, &error.message);
                    return Err(error);
                }
            }
        }
        Ok(())
    })
}

/// Follow renames any client makes, from the start of the connection.
///
/// Subscribes to `phux.session.name/v1` now if `HELLO_OK` has been applied,
/// else as soon as it is. Once per client; idempotent. Against a server that
/// advertises `ORGANIZATION` the key is never subscribed: a client that has
/// not attached subscribes the server-wide event stream once the replica is
/// live, and the replica applies each `SessionRenamed` to the session list.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_follow_session_names(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if client.detached {
            return Err(BridgeError::state("the connection has ended"));
        }
        client.session_rename.follow = true;
        if client.protocol_ready {
            subscribe(client)?;
        }
        Ok(())
    })
}

/// The latest rename's state, as `phux_client_session_rename_info` reports it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSessionRenameInfo {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub status: u32,
    pub session_id: u32,
    pub sessions_revision: u64,
    pub message: PhuxBytes,
}

/// Read the latest rename's request ID, status and reason.
///
/// # Safety
/// Client is live and unmodified for the call; `out` is writable and its
/// `size`/`version` are initialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_rename_info(
    client: *const PhuxClient,
    out: *mut PhuxSessionRenameInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxSessionRenameInfo>(),
            out.version,
        )?;
        let rename = &client.session_rename;
        *out = PhuxSessionRenameInfo {
            size: out.size,
            version: out.version,
            request_id: rename.request_id,
            status: rename.status,
            session_id: rename.session_id,
            sessions_revision: rename.sessions_revision,
            message: bytes_out(&rename.message),
        };
        Ok(())
    })
}
