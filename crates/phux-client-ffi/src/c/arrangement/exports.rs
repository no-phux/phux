use super::{
    mutation, start_read,
    types::{
        PhuxArrangementGroup, PhuxArrangementInfo, PhuxArrangementMutation, PhuxArrangementNode,
        PhuxCatalogTerminal,
    },
};
use crate::c::error::{BridgeError, check_struct};
use crate::c::{
    PhuxClient, PhuxClientResult, PhuxResourceId, bytes_out, terminal_id_out, with_client_mut,
    with_client_ref,
};
use std::mem;

/// Queue a bounded, emulator-free server catalog and selected topology refresh.
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_arrangement_refresh(
    client: *mut PhuxClient,
    request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        client.operations.check_request_id(request_id)?;
        start_read(client, request_id, None)?;
        client.operations.consume_request_id(request_id);
        Ok(())
    })
}

/// Publish a typed edit, then read it back.
///
/// The edit is a compare-and-swap `ARRANGEMENT_REPLACE` of the stored
/// arrangement at its revision when the publication came from one, else a
/// last-write-wins legacy `SET`.
/// # Safety
/// Client is exclusively accessed. Input is readable with all nonempty spans
/// valid for this call. No pointer may alias client storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_arrangement_mutate(
    client: *mut PhuxClient,
    input: *const PhuxArrangementMutation,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies the readable input record when non-null.
        let input =
            unsafe { input.as_ref() }.ok_or_else(|| BridgeError::invalid("mutation is null"))?;
        client.operations.check_request_id(input.request_id)?;
        // SAFETY: forwards the sized input and readable span contract.
        let next = unsafe { mutation::prepare(&client.arrangement, input) }?;
        let proposal = super::propose(client, next)?;
        start_read(client, input.request_id, Some(proposal))?;
        client.operations.consume_request_id(input.request_id);
        Ok(())
    })
}

/// Read the snapshot header and latest transaction state.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_arrangement_info(
    client: *const PhuxClient,
    out: *mut PhuxArrangementInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxArrangementInfo>(), out.version)?;
        let ws = &client.arrangement;
        *out = PhuxArrangementInfo {
            revision: ws.revision,
            session_id: ws.selected,
            state: ws.state,
            group_count: count(ws.topology.groups.len())?,
            node_count: count(ws.nodes.len())?,
            terminal_count: count(ws.catalog.terminals.len())?,
            request_id: ws.request,
            status: ws.status,
            message: bytes_out(&ws.message),
            backend: ws.backend_code(),
            server_revision: ws.server_revision(),
            session_key: super::selected_key(client).map_or([0; 16], |key| *key.as_bytes()),
            arrangement_key: ws.stored_key_bytes(),
            unarranged_count: count(ws.unarranged())?,
            unplaced_count: count(ws.unplaced())?,
            ..PhuxArrangementInfo::default()
        };
        Ok(())
    })
}

/// Read one stable shared group; its name is borrowed until mutation.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_arrangement_group_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxArrangementGroup,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxArrangementGroup>(),
            out.version,
        )?;
        *out = PhuxArrangementGroup::default();
        let ws = &client.arrangement;
        let group = ws.topology.groups.get(index).ok_or_else(no_value)?;
        *out = PhuxArrangementGroup {
            group_id: group.id,
            name: bytes_out(group.name.as_bytes()),
            root_node: ws.roots[index],
            ..PhuxArrangementGroup::default()
        };
        Ok(())
    })
}

/// Read one flattened split or terminal leaf.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_arrangement_node_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxArrangementNode,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxArrangementNode>(), out.version)?;
        *out = PhuxArrangementNode::default();
        let node = client.arrangement.nodes.get(index).ok_or_else(no_value)?;
        *out = PhuxArrangementNode {
            kind: node.kind,
            terminal_id: node
                .terminal
                .as_ref()
                .map_or_else(PhuxResourceId::default, terminal_id_out),
            first: node.first,
            second: node.second,
            ratio: node.ratio,
            ..PhuxArrangementNode::default()
        };
        Ok(())
    })
}

/// Read a durable terminal from any session without subscribing or allocating an emulator.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_catalog_terminal_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxCatalogTerminal,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxCatalogTerminal>(), out.version)?;
        *out = PhuxCatalogTerminal::default();
        let terminal = client
            .arrangement
            .catalog
            .terminals
            .get(index)
            .ok_or_else(no_value)?;
        *out = PhuxCatalogTerminal {
            terminal_id: terminal_id_out(&terminal.id),
            session_id: terminal.session,
            title: bytes_out(terminal.title.as_bytes()),
            cwd: bytes_out(terminal.cwd.as_bytes()),
            ..PhuxCatalogTerminal::default()
        };
        Ok(())
    })
}

fn count(count: usize) -> Result<u32, BridgeError> {
    u32::try_from(count).map_err(|_| BridgeError::state("arrangement count overflow"))
}

const fn no_value() -> BridgeError {
    BridgeError {
        result: PhuxClientResult::NoValue,
        message: String::new(),
    }
}
