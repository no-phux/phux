//! Rebuilding a Terminal across a graceful upgrade (ADR-0032).

use phux_core::ids::ResourceId;
use tokio_util::sync::CancellationToken;

use phux_terminal::{TerminalActor, TerminalActorBundle};

use crate::state::SharedState;
use crate::state::upgrade_blob::{RebuildError, RebuiltPane, pane_seed};
use crate::upgrade::blob::PaneBlob;

/// Build a pane actor around its inherited PTY, or a PTY-less actor that
/// replays its snapshot. Only one of the two PTY ids is corruption.
fn pane_actor_bundle(
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<TerminalActorBundle, RebuildError> {
    let seed = pane_seed(p);
    let boxed = |error| RebuildError::Actor(Box::new(error));
    Ok(match (p.master_fd, p.child_pid) {
        (Some(master_fd), Some(child_pid)) => TerminalActor::new_with_adopted_pty(
            master_fd,
            child_pid,
            p.cols,
            p.rows,
            scrollback,
            CancellationToken::new(),
            &seed,
        )
        .map_err(boxed)?,
        (None, None) => TerminalActor::new_with_seed(p.cols, p.rows, &seed).map_err(boxed)?,
        _ => {
            return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id });
        }
    })
}

/// Rebuild one pane, letting `wire` install its sinks before the actor runs.
pub(crate) fn rebuild_pane<W>(
    core: ResourceId,
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
    wire: impl FnOnce(ResourceId, &mut TerminalActor) -> W,
) -> Result<RebuiltPane<W>, RebuildError> {
    let TerminalActorBundle {
        mut actor,
        handle,
        token,
        exit_notify,
        ..
    } = pane_actor_bundle(p, scrollback)?;
    let wired = wire(core, &mut actor);
    Ok(RebuiltPane {
        handle,
        token,
        engine: Box::pin(actor.run()),
        exit_notify,
        wired,
    })
}

/// A resumed pane is wired exactly as a fresh spawn is (event sink, agent
/// detector), so it keeps its events and agent-state feed.
pub(crate) fn wired(
    state: &SharedState,
) -> impl FnMut(
    ResourceId,
    &PaneBlob,
    phux_config::ScrollbackLimits,
) -> Result<RebuiltPane<crate::runtime::PaneWiring>, RebuildError>
+ '_ {
    move |core, p, scrollback| {
        rebuild_pane(core, p, scrollback, |pane, actor| {
            crate::runtime::commands::wire_pane_actor(state, pane, actor)
        })
    }
}

/// A rebuild with no wiring, for tests of the tree alone.
#[cfg(test)]
pub(crate) fn unwired(
    core: ResourceId,
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<RebuiltPane<()>, RebuildError> {
    rebuild_pane(core, p, scrollback, |_, _| ())
}
