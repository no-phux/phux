use super::{PhuxWorkspaceMutation, SharedWorkspace, model};
use crate::error::{BridgeError, bytes_in, check_struct, terminal_id_in};
use phux_client_core::layout::{self, LayoutNode, WindowState, Workspace};
use phux_protocol::TerminalId;
use std::{mem, ptr};

pub(super) unsafe fn prepare(
    ws: &SharedWorkspace,
    input: &PhuxWorkspaceMutation,
) -> Result<Workspace, BridgeError> {
    validate_revision(ws, input)?;
    let mut next = ws.topology.clone();
    // SAFETY: caller supplies the new identity span.
    unsafe { unplace_fallback_seed(ws, &mut next, input) }?;
    // SAFETY: caller supplies a validated sized record with readable spans.
    unsafe { edit(ws, &mut next, input) }?;
    next.prune_empty_windows();
    model::preserve_focus(&mut next, &ws.topology);
    model::flatten(&next)?;
    Ok(next)
}

fn validate_revision(
    ws: &SharedWorkspace,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    check_struct(
        input.size,
        mem::size_of::<PhuxWorkspaceMutation>(),
        input.version,
    )?;
    ws.busy()?;
    if input.session_id != ws.selected || input.expected_revision != ws.revision {
        return Err(BridgeError::state("stale workspace session or revision"));
    }
    if ws.state == 3 {
        return Err(BridgeError::state(
            "refresh valid shared metadata before editing",
        ));
    }
    validate_span_bounds(input)?;
    Ok(())
}

fn validate_span_bounds(input: &PhuxWorkspaceMutation) -> Result<(), BridgeError> {
    if [
        input.name.len,
        input.terminal_id.host.len,
        input.new_terminal_id.host.len,
    ]
    .into_iter()
    .any(|len| len > model::MAX_TEXT)
    {
        return Err(BridgeError::invalid(
            "workspace input text exceeds 4096 bytes",
        ));
    }
    Ok(())
}

unsafe fn unplace_fallback_seed(
    ws: &SharedWorkspace,
    next: &mut Workspace,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    if ws.state == 1 && matches!(input.kind, 1 | 2) {
        // SAFETY: caller supplies the new identity span.
        let seed = if input.kind == 1 {
            &input.terminal_id
        } else {
            &input.new_terminal_id
        };
        let id = unsafe { new_terminal(ws, seed) }?;
        next.windows
            .retain(|w| w.state.tree != Some(LayoutNode::Leaf(id.clone())));
    }
    Ok(())
}

unsafe fn edit(
    ws: &SharedWorkspace,
    next: &mut Workspace,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    if input.kind == 1 {
        // SAFETY: forwards the input record's readable spans.
        return unsafe { add_window(ws, next, input) };
    }
    let index = next
        .windows
        .iter()
        .position(|w| w.id == input.window_id)
        .ok_or_else(|| BridgeError::state("layout window no longer exists"))?;
    match input.kind {
        2 => {
            // SAFETY: forwards input identity spans.
            unsafe { split(ws, &mut next.windows[index], input) }?;
        }
        3 => {
            // SAFETY: input identity span is readable.
            unsafe { remove_terminal(&mut next.windows[index], input) }?;
        }
        4 => reorder(next, index, input.index as usize)?,
        5 => resize(&mut next.windows[index], input)?,
        6 => {
            // SAFETY: input name span is readable.
            next.windows[index].name = unsafe { name(input) }?;
        }
        7 => {
            next.windows.remove(index);
        }
        _ => return Err(BridgeError::invalid("unknown workspace mutation kind")),
    }
    Ok(())
}

unsafe fn add_window(
    ws: &SharedWorkspace,
    next: &mut Workspace,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    // SAFETY: forwards the input record's readable identity/span contract.
    let id = unsafe { new_terminal(ws, &input.terminal_id) }?;
    // SAFETY: name belongs to the input record.
    let name = unsafe { name(input) }?;
    next.add_window(name, id);
    Ok(())
}

unsafe fn remove_terminal(
    window: &mut WindowState,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    // SAFETY: input identity span is readable.
    let id = unsafe { terminal_id_in(ptr::from_ref(&input.terminal_id)) }?;
    window.state.tree = layout::kill_pane(tree(window)?, &id).map_err(layout_error)?;
    Ok(())
}

unsafe fn name(input: &PhuxWorkspaceMutation) -> Result<String, BridgeError> {
    if input.name.len > model::MAX_TEXT {
        return Err(BridgeError::invalid("window name too long"));
    }
    // SAFETY: caller provides the bounded name span.
    let bytes = unsafe { bytes_in(input.name.data, input.name.len) }?;
    let name =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("window name is not UTF-8"))?;
    model::text(name)?;
    Ok(name.to_owned())
}

unsafe fn new_terminal(
    ws: &SharedWorkspace,
    input: &crate::PhuxTerminalId,
) -> Result<TerminalId, BridgeError> {
    if input.host.len > model::MAX_TEXT {
        return Err(BridgeError::invalid("terminal host too long"));
    }
    // SAFETY: caller supplies the identity and bounded host.
    let id = unsafe { terminal_id_in(ptr::from_ref(input)) }?;
    if !ws.catalog.allows(&id, ws.selected) {
        return Err(BridgeError::state(
            "new terminal is absent from selected catalog; refresh after spawn",
        ));
    }
    if ws.state != 1 && ws.nodes.iter().any(|n| n.terminal.as_ref() == Some(&id)) {
        return Err(BridgeError::state(
            "terminal is already placed in shared topology",
        ));
    }
    Ok(id)
}

unsafe fn split(
    ws: &SharedWorkspace,
    window: &mut WindowState,
    input: &PhuxWorkspaceMutation,
) -> Result<(), BridgeError> {
    // SAFETY: caller supplies both terminal identities.
    let id = unsafe { new_terminal(ws, &input.new_terminal_id) }?;
    // SAFETY: caller supplies the target identity.
    let target = unsafe { terminal_id_in(ptr::from_ref(&input.terminal_id)) }?;
    let dir = match input.direction {
        2 => layout::SplitDir::Horizontal,
        3 => layout::SplitDir::Vertical,
        _ => return Err(BridgeError::invalid("invalid split direction")),
    };
    window.state.tree = Some(
        layout::split_at(tree(window)?, &target, &id, dir, input.ratio).map_err(layout_error)?,
    );
    Ok(())
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the layout error"
)]
fn layout_error(error: layout::LayoutError) -> BridgeError {
    BridgeError::invalid(error.to_string())
}

fn tree(window: &WindowState) -> Result<&LayoutNode, BridgeError> {
    window
        .state
        .tree
        .as_ref()
        .ok_or_else(|| BridgeError::state("window has no tree"))
}

fn reorder(next: &mut Workspace, from: usize, to: usize) -> Result<(), BridgeError> {
    if to >= next.windows.len() {
        return Err(BridgeError::invalid("reorder index out of bounds"));
    }
    let window = next.windows.remove(from);
    next.windows.insert(to, window);
    Ok(())
}

fn resize(window: &mut WindowState, input: &PhuxWorkspaceMutation) -> Result<(), BridgeError> {
    if input.path_len > 64 || !input.ratio.is_finite() || input.ratio <= 0.0 || input.ratio >= 1.0 {
        return Err(BridgeError::invalid("invalid resize path or ratio"));
    }
    let mut node = window
        .state
        .tree
        .as_mut()
        .ok_or_else(|| BridgeError::state("empty window"))?;
    for depth in 0..input.path_len {
        let LayoutNode::Split { left, right, .. } = node else {
            return Err(BridgeError::state("stale split path"));
        };
        node = if input.path_bits & (1 << depth) == 0 {
            left
        } else {
            right
        };
    }
    let LayoutNode::Split { ratio, .. } = node else {
        return Err(BridgeError::state("path does not name a split"));
    };
    *ratio = input.ratio;
    Ok(())
}
