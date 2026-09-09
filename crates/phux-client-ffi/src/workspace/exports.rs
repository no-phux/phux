use super::{
    mutation, start_read,
    types::{
        PhuxCatalogTerminal, PhuxWorkspaceInfo, PhuxWorkspaceMutation, PhuxWorkspaceNode,
        PhuxWorkspaceWindow,
    },
};
use crate::error::{BridgeError, check_struct};
use crate::{
    PhuxClient, PhuxClientResult, PhuxResourceId, bytes_out, terminal_id_out, with_client_mut,
    with_client_ref,
};
use std::mem;

/// Queue a bounded, emulator-free server catalog and selected topology refresh.
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_workspace_refresh(
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

/// Publish a typed topology edit by whole-value LWW SET and confirming GET.
/// # Safety
/// Client is exclusively accessed. Input is readable with all nonempty spans
/// valid for this call. No pointer may alias client storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_workspace_mutate(
    client: *mut PhuxClient,
    input: *const PhuxWorkspaceMutation,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.ensure_attached()?;
        // SAFETY: caller supplies the readable input record when non-null.
        let input =
            unsafe { input.as_ref() }.ok_or_else(|| BridgeError::invalid("mutation is null"))?;
        client.operations.check_request_id(input.request_id)?;
        // SAFETY: forwards the sized input and readable span contract.
        let next = unsafe { mutation::prepare(&client.workspace, input) }?;
        let bytes = next
            .encode_topology_cbor()
            .map_err(|e| BridgeError::invalid(e.to_string()))?;
        if bytes.len() > 256 * 1024 {
            return Err(BridgeError::state("layout metadata exceeds 256 KiB"));
        }
        start_read(client, input.request_id, Some((next, bytes)))?;
        client.operations.consume_request_id(input.request_id);
        Ok(())
    })
}

/// Read the snapshot header and latest transaction state.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_workspace_info(
    client: *const PhuxClient,
    out: *mut PhuxWorkspaceInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxWorkspaceInfo>(), out.version)?;
        let ws = &client.workspace;
        *out = PhuxWorkspaceInfo {
            revision: ws.revision,
            session_id: ws.selected,
            state: ws.state,
            window_count: count(ws.topology.windows.len())?,
            node_count: count(ws.nodes.len())?,
            terminal_count: count(ws.catalog.terminals.len())?,
            request_id: ws.request,
            status: ws.status,
            message: bytes_out(&ws.message),
            ..PhuxWorkspaceInfo::default()
        };
        Ok(())
    })
}

/// Read one stable shared window; its name is borrowed until mutation.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_workspace_window_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxWorkspaceWindow,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxWorkspaceWindow>(), out.version)?;
        *out = PhuxWorkspaceWindow::default();
        let ws = &client.workspace;
        let window = ws.topology.windows.get(index).ok_or_else(no_value)?;
        *out = PhuxWorkspaceWindow {
            window_id: window.id,
            name: bytes_out(window.name.as_bytes()),
            root_node: ws.roots[index],
            ..PhuxWorkspaceWindow::default()
        };
        Ok(())
    })
}

/// Read one flattened split or terminal leaf.
/// # Safety
/// Client is live and unmodified; output is initialized, writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_workspace_node_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxWorkspaceNode,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxWorkspaceNode>(), out.version)?;
        *out = PhuxWorkspaceNode::default();
        let node = client.workspace.nodes.get(index).ok_or_else(no_value)?;
        *out = PhuxWorkspaceNode {
            kind: node.kind,
            terminal_id: node
                .terminal
                .as_ref()
                .map_or_else(PhuxResourceId::default, terminal_id_out),
            first: node.first,
            second: node.second,
            ratio: node.ratio,
            ..PhuxWorkspaceNode::default()
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
            .workspace
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
    u32::try_from(count).map_err(|_| BridgeError::state("workspace count overflow"))
}

const fn no_value() -> BridgeError {
    BridgeError {
        result: PhuxClientResult::NoValue,
        message: String::new(),
    }
}
