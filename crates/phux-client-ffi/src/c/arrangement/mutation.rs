use super::{PhuxArrangementMutation, SharedArrangement, model};
use crate::c::error::{BridgeError, bytes_in, check_struct, terminal_id_in};
use phux_client_core::layout::{self, ArrangementDoc, Group, LayoutNode};
use phux_protocol::ResourceId;
use std::{mem, ptr};

pub(super) unsafe fn prepare(
    ws: &SharedArrangement,
    input: &PhuxArrangementMutation,
) -> Result<ArrangementDoc, BridgeError> {
    validate_revision(ws, input)?;
    let mut next = ws.topology.clone();
    // SAFETY: caller supplies a validated sized record with readable spans.
    unsafe { edit(ws, &mut next, input) }?;
    if input.kind != 3 {
        next.prune_empty_groups();
        model::preserve_focus(&mut next, &ws.topology);
    }
    model::flatten(&next)?;
    Ok(next)
}

fn validate_revision(
    ws: &SharedArrangement,
    input: &PhuxArrangementMutation,
) -> Result<(), BridgeError> {
    check_struct(
        input.size,
        mem::size_of::<PhuxArrangementMutation>(),
        input.version,
    )?;
    ws.busy()?;
    if input.session_id != ws.selected || input.expected_revision != ws.revision {
        return Err(BridgeError::state("stale arrangement session or revision"));
    }
    if ws.state == 3 {
        return Err(BridgeError::state(
            "refresh valid shared metadata before editing",
        ));
    }
    validate_span_bounds(input)?;
    Ok(())
}

fn validate_span_bounds(input: &PhuxArrangementMutation) -> Result<(), BridgeError> {
    if [
        input.name.len,
        input.terminal_id.host.len,
        input.new_terminal_id.host.len,
    ]
    .into_iter()
    .any(|len| len > model::MAX_TEXT)
    {
        return Err(BridgeError::invalid(
            "arrangement input text exceeds 4096 bytes",
        ));
    }
    Ok(())
}

unsafe fn edit(
    ws: &SharedArrangement,
    next: &mut ArrangementDoc,
    input: &PhuxArrangementMutation,
) -> Result<(), BridgeError> {
    if input.kind == 1 {
        // SAFETY: forwards the input record's readable spans.
        return unsafe { add_group(ws, next, input) };
    }
    if input.kind == 3 {
        // SAFETY: input identity span is readable.
        return unsafe { remove_terminal(next, input) };
    }
    let index = next
        .groups
        .iter()
        .position(|w| w.id == input.group_id)
        .ok_or_else(|| BridgeError::state("arrangement group no longer exists"))?;
    match input.kind {
        2 => {
            // SAFETY: forwards input identity spans.
            unsafe { split(ws, &mut next.groups[index], input) }?;
        }
        4 => reorder(next, index, input.index as usize)?,
        5 => resize(&mut next.groups[index], input)?,
        6 => {
            // SAFETY: input name span is readable.
            next.groups[index].name = unsafe { name(input) }?;
        }
        7 => {
            next.groups.remove(index);
        }
        _ => return Err(BridgeError::invalid("unknown arrangement mutation kind")),
    }
    Ok(())
}

unsafe fn add_group(
    ws: &SharedArrangement,
    next: &mut ArrangementDoc,
    input: &PhuxArrangementMutation,
) -> Result<(), BridgeError> {
    // SAFETY: forwards the input record's readable identity/span contract.
    let id = unsafe { new_terminal(ws, &input.terminal_id) }?;
    // SAFETY: name belongs to the input record.
    let name = unsafe { name(input) }?;
    next.add_group(name, id);
    Ok(())
}

unsafe fn remove_terminal(
    next: &mut ArrangementDoc,
    input: &PhuxArrangementMutation,
) -> Result<(), BridgeError> {
    // SAFETY: input identity span is readable.
    let id = unsafe { terminal_id_in(ptr::from_ref(&input.terminal_id)) }?;
    next.close_pane(&id).map_err(close_error)
}

unsafe fn name(input: &PhuxArrangementMutation) -> Result<String, BridgeError> {
    if input.name.len > model::MAX_TEXT {
        return Err(BridgeError::invalid("group name too long"));
    }
    // SAFETY: caller provides the bounded name span.
    let bytes = unsafe { bytes_in(input.name.data, input.name.len) }?;
    let name =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("group name is not UTF-8"))?;
    model::text(name)?;
    Ok(name.to_owned())
}

unsafe fn new_terminal(
    ws: &SharedArrangement,
    input: &crate::c::PhuxResourceId,
) -> Result<ResourceId, BridgeError> {
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
    if ws.nodes.iter().any(|n| n.terminal.as_ref() == Some(&id)) {
        return Err(BridgeError::state(
            "terminal is already placed in shared topology",
        ));
    }
    Ok(id)
}

unsafe fn split(
    ws: &SharedArrangement,
    group: &mut Group,
    input: &PhuxArrangementMutation,
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
    group.state.tree =
        Some(layout::split_at(tree(group)?, &target, &id, dir, input.ratio).map_err(layout_error)?);
    Ok(())
}

fn close_error(error: layout::LayoutError) -> BridgeError {
    match error {
        layout::LayoutError::LastPane => {
            BridgeError::invalid("cannot close the final pane in a persisted layout")
        }
        other => layout_error(other),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "Result::map_err consumes the layout error"
)]
fn layout_error(error: layout::LayoutError) -> BridgeError {
    BridgeError::invalid(error.to_string())
}

fn tree(group: &Group) -> Result<&LayoutNode, BridgeError> {
    group
        .state
        .tree
        .as_ref()
        .ok_or_else(|| BridgeError::state("group has no tree"))
}

fn reorder(next: &mut ArrangementDoc, from: usize, to: usize) -> Result<(), BridgeError> {
    if to >= next.groups.len() {
        return Err(BridgeError::invalid("reorder index out of bounds"));
    }
    let group = next.groups.remove(from);
    next.groups.insert(to, group);
    Ok(())
}

fn resize(group: &mut Group, input: &PhuxArrangementMutation) -> Result<(), BridgeError> {
    if input.path_len > 64 || !input.ratio.is_finite() || input.ratio <= 0.0 || input.ratio >= 1.0 {
        return Err(BridgeError::invalid("invalid resize path or ratio"));
    }
    let mut node = group
        .state
        .tree
        .as_mut()
        .ok_or_else(|| BridgeError::state("empty group"))?;
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
