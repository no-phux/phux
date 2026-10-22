//! C exports: per-request results and per-target outcomes.

use super::{
    BridgeError, PhuxBytes, PhuxClient, PhuxClientResult, PhuxOrganizationInfo, PhuxResourceRecord,
    PhuxSessionRecord, bytes_out, check_struct, mem, with_client_mut, with_client_ref,
};

macro_rules! sized_defaults {
    ($($ty:ty),*) => {$(
        impl Default for $ty {
            /// A zeroed record whose `size` and `version` are initialized, as
            /// every getter requires.
            fn default() -> Self {
                // SAFETY: every field is an integer, a bool, a byte array, or
                // a nullable span or id whose all-zero value is valid.
                let mut value: Self = unsafe { std::mem::zeroed() };
                value.size = mem::size_of::<Self>();
                value.version = crate::c::ABI_VERSION;
                value
            }
        }
    )*};
}

// ----- exports: results --------------------------------------------------------

/// One request's result.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrgResult {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub kind: u32,
    pub status: u32,
    pub refusal: u32,
    pub undoable: bool,
    pub revision: u64,
    pub conflict_revision: u64,
    pub target_count: u32,
    pub message: PhuxBytes,
}

/// One target of a request's result.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxOrgTarget {
    pub size: usize,
    pub version: u32,
    pub key: [u8; 16],
    pub is_session: bool,
    pub status: u32,
    pub refusal: u32,
    pub message: PhuxBytes,
}

/// How many organization results are retained, pending ones included.
///
/// # Safety
/// `client`, when non-null, is live and unmodified for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_count(client: *const PhuxClient) -> usize {
    // SAFETY: forwards the caller's pointer contract.
    unsafe { client.as_ref() }.map_or(0, |client| client.inner.org.requests.len())
}

/// Read the result at `index`, in request-id order.
///
/// # Safety
/// Client is live and unmodified; `out` is writable with size/version
/// initialized. `message` borrows the client until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxOrgResult,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxOrgResult>(), out.version)?;
        let (request_id, request) = client
            .org
            .requests
            .iter()
            .nth(index)
            .ok_or_else(|| BridgeError::invalid("result index out of range"))?;
        let view = &request.view;
        *out = PhuxOrgResult {
            size: out.size,
            version: out.version,
            request_id: *request_id,
            kind: request.kind as u32,
            status: view.status,
            refusal: view.refusal,
            undoable: view.undoable,
            revision: view.revision,
            conflict_revision: view.conflict_revision,
            target_count: u32::try_from(view.targets.len()).unwrap_or(u32::MAX),
            message: bytes_out(&view.message),
        };
        Ok(())
    })
}

/// Read one target of the result at `result_index`.
///
/// # Safety
/// As `phux_client_org_result_get`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_target_get(
    client: *const PhuxClient,
    result_index: usize,
    target_index: usize,
    out: *mut PhuxOrgTarget,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxOrgTarget>(), out.version)?;
        let request = client
            .org
            .requests
            .values()
            .nth(result_index)
            .ok_or_else(|| BridgeError::invalid("result index out of range"))?;
        let target = request
            .view
            .targets
            .get(target_index)
            .ok_or_else(|| BridgeError::invalid("target index out of range"))?;
        *out = PhuxOrgTarget {
            size: out.size,
            version: out.version,
            key: target.key,
            is_session: target.is_session,
            status: target.status,
            refusal: target.refusal,
            message: bytes_out(&target.message),
        };
        Ok(())
    })
}

/// Release a settled result. A pending one is refused: its outcome is still
/// owed to the host.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_org_result_release(
    client: *mut PhuxClient,
    request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let request = client
            .org
            .requests
            .get(&request_id)
            .ok_or_else(|| BridgeError::invalid("unknown organization request"))?;
        if request.pending() {
            return Err(BridgeError::state("the request is still pending"));
        }
        client.org.requests.remove(&request_id);
        Ok(())
    })
}

sized_defaults!(
    PhuxOrganizationInfo,
    PhuxSessionRecord,
    PhuxResourceRecord,
    PhuxOrgResult,
    PhuxOrgTarget
);
