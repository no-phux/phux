//! C exports: organization state and session/resource records.

use super::{
    BridgeError, ExitPolicy, PHUX_SESSION_RECORD_AWAITING_ORGANIZATION, PHUX_SESSION_RECORD_EMPTY,
    PHUX_SESSION_RECORD_FOCUSED, PHUX_SESSION_RECORD_INTAKE, PHUX_SESSION_RECORD_ORGANIZED,
    PhuxBytes, PhuxClient, PhuxClientResult, PhuxResourceId, ReplicaState, bytes_out, check_struct,
    exec_code, lifetime_code, mem, outcome_code, supported, with_client_mut, with_client_ref,
};

// ----- exports: state and records ------------------------------------------------

/// Whether `HELLO_OK` advertised `ORGANIZATION`.
///
/// # Safety
/// Client is live and unmodified for the call; `out_supported` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies a writable output when non-null.
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("supported output is null"))?;
        *out = supported(client);
        Ok(())
    })
}

/// The replica's state, authority, Intake, and change counter.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrganizationInfo {
    pub size: usize,
    pub version: u32,
    pub supported: bool,
    pub state: u32,
    pub authority: [u8; 16],
    pub inbox: [u8; 16],
    pub revision: u64,
    pub record_count: u32,
    pub message: PhuxBytes,
}

/// Read the organization replica's state.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. `message` borrows the client until the next mutable call.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the control guard lives exactly as long as the replica borrow it lends"
)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_info(
    client: *const PhuxClient,
    out: *mut PhuxOrganizationInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxOrganizationInfo>(),
            out.version,
        )?;
        let supported = supported(client);
        let (state, authority, inbox) = {
            let control = client.control();
            let replica = control.organization();
            let state = if supported {
                match replica.state() {
                    ReplicaState::Empty => 0,
                    ReplicaState::Seeding => 1,
                    ReplicaState::Live => 2,
                    ReplicaState::Stale => 3,
                }
            } else {
                0
            };
            (
                state,
                replica.authority().map_or([0; 16], |key| *key.as_bytes()),
                replica.intake().map_or([0; 16], |key| *key.as_bytes()),
            )
        };
        *out = PhuxOrganizationInfo {
            size: out.size,
            version: out.version,
            supported,
            state,
            authority,
            inbox,
            revision: client.org.revision,
            record_count: u32::try_from(client.org.records.len()).unwrap_or(u32::MAX),
            message: bytes_out(&client.org.message),
        };
        Ok(())
    })
}

/// Reseed the replica from the first page.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_organization_refresh(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !supported(client) || client.detached {
            return Err(BridgeError::state(
                "the server does not advertise ORGANIZATION",
            ));
        }
        if client.control().refresh_organization().is_none() {
            return Err(BridgeError::state("the connection is not negotiated"));
        }
        let epoch = client.control().connection_epoch();
        client.org.seeded_epoch = Some(epoch);
        client.process_runtime_events()?;
        Ok(())
    })
}

/// One session's full record, beside the frozen `PhuxSessionInfo` at the
/// same index.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSessionRecord {
    pub size: usize,
    pub version: u32,
    pub session_id: u32,
    pub key: [u8; 16],
    pub arrangement_key: [u8; 16],
    pub revision: u64,
    pub created_at_unix_secs: i64,
    pub lifetime: u32,
    pub flags: u32,
    pub member_count: u32,
    pub running_count: u32,
    pub completed_count: u32,
    pub lost_count: u32,
    pub attached_client_count: u16,
    pub name: PhuxBytes,
    pub key_text: PhuxBytes,
}

/// Read the full record of the session `phux_client_session_get` reads at
/// the same index.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. Spans borrow the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_record_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxSessionRecord,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxSessionRecord>(), out.version)?;
        let session = client
            .sessions
            .get(index)
            .ok_or_else(|| BridgeError::invalid("session index out of range"))?;
        let org = &session.org;
        let members = if org.known {
            org.members
        } else {
            session.snapshot_members
        };
        let mut flags = 0;
        if session.focused {
            flags |= PHUX_SESSION_RECORD_FOCUSED;
        }
        if org.awaiting_organization {
            flags |= PHUX_SESSION_RECORD_AWAITING_ORGANIZATION;
        }
        if org.inbox {
            flags |= PHUX_SESSION_RECORD_INTAKE;
        }
        if org.known {
            flags |= PHUX_SESSION_RECORD_ORGANIZED;
        }
        if members == 0 {
            flags |= PHUX_SESSION_RECORD_EMPTY;
        }
        *out = PhuxSessionRecord {
            size: out.size,
            version: out.version,
            session_id: session.session_id,
            key: session.key.map_or([0; 16], |key| *key.as_bytes()),
            arrangement_key: org.arrangement.map_or([0; 16], |key| *key.as_bytes()),
            revision: org.revision,
            created_at_unix_secs: session.created_at_unix_secs,
            lifetime: lifetime_code(session.lifetime),
            flags,
            member_count: members,
            running_count: org.running,
            completed_count: org.completed,
            lost_count: org.lost,
            attached_client_count: session.attached_client_count,
            name: bytes_out(&session.name),
            key_text: bytes_out(&session.key_text),
        };
        Ok(())
    })
}

/// How many resource records the replica holds.
///
/// # Safety
/// `client`, when non-null, is live and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_record_count(client: *const PhuxClient) -> usize {
    // SAFETY: forwards the caller's pointer contract.
    unsafe { client.as_ref() }.map_or(0, |client| client.inner.org.records.len())
}

/// One resource record, live or not.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxResourceRecord {
    pub size: usize,
    pub version: u32,
    pub kind: u32,
    pub key: [u8; 16],
    pub session_key: [u8; 16],
    pub revision: u64,
    pub activation_id: u64,
    pub exec_state: u32,
    pub outcome_kind: u32,
    pub outcome_code: i32,
    pub on_exit: u32,
    pub live: bool,
    pub live_id: PhuxResourceId,
    pub session_id: u32,
    pub name: PhuxBytes,
    pub key_text: PhuxBytes,
}

/// Read one resource record.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. Spans borrow the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_record_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxResourceRecord,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxResourceRecord>(), out.version)?;
        let view = client
            .org
            .records
            .get(index)
            .ok_or_else(|| BridgeError::invalid("record index out of range"))?;
        let record = &view.record;
        let activation = record.activation;
        let (outcome_kind, outcome_code) =
            outcome_code(activation.and_then(|activation| activation.outcome));
        *out = PhuxResourceRecord {
            size: out.size,
            version: out.version,
            kind: u32::from(record.kind.as_wire()),
            key: *record.key.as_bytes(),
            session_key: *record.session.as_bytes(),
            revision: record.revision.get(),
            activation_id: activation.map_or(0, |activation| activation.id.get()),
            exec_state: exec_code(activation.map(|activation| activation.state)),
            outcome_kind,
            outcome_code,
            on_exit: match record.on_exit {
                ExitPolicy::Remove => 0,
                ExitPolicy::Retain => 1,
            },
            live: record.live.is_some(),
            live_id: view.live_view(),
            session_id: view.session_id,
            name: bytes_out(&view.name),
            key_text: bytes_out(&view.key_text),
        };
        Ok(())
    })
}
