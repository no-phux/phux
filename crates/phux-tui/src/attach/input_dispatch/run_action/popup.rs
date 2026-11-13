//! `popup`: one command in a floating box over the panes, gone when it
//! succeeds and held on a failure until a key.

use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{FrameKind, SpawnResource};

use crate::attach::floating::floating_box;
use crate::attach::input_dispatch::args::str_arg;
use crate::attach::input_dispatch::dispatch::spawn_initial_size;
use crate::render::overlay::PromptOverlay;

use super::{ActionCall, bind_satellite_spawn, spawn_place};

/// Run `command` through `sh -c` in a floating pane, in the focused pane's
/// directory and on its machine, or prompt for the command. The box is the
/// one plugin overlay panes use: any action or the command's success closes
/// it. Retained so a failure stays readable; a server without retention
/// closes it on any exit.
pub(in crate::attach) fn popup(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    let Some(command) = str_arg(resolved, "command").filter(|c| !c.trim().is_empty()) else {
        ctx.overlays.push(Box::new(PromptOverlay::popup(ctx.theme)));
        return;
    };
    let request_id = ctx.take_request_id();
    let place = spawn_place(resolved, focused, panes);
    let mut frame = FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: Some(vec!["/bin/sh".to_owned(), "-c".to_owned(), command.clone()]),
        cwd: place.directory,
        env: None,
        term: None,
        satellite: place.host.map(SatelliteHost::new),
        owner_terminal: None,
        agent_session: None,
        initial_size: spawn_initial_size(ctx, |content| {
            let inner = floating_box(content).inner;
            Some((inner.w, inner.h))
        }),
        resource: Some(Box::new(SpawnResource::default().with_retain_secs(Some(0)))),
    };
    bind_satellite_spawn(&mut frame);
    effects.spawn_floating = Some((request_id, command, frame));
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use phux_protocol::wire::frame::FrameKind;

    use crate::attach::input_dispatch::args::bare_action;
    use crate::attach::input_dispatch::run_action::run_action;
    use crate::attach::input_dispatch::test_support::{fx, tid};
    use crate::layout::ArrangementDoc;

    #[test]
    fn a_command_spawns_floating_and_a_bare_popup_prompts() {
        let mut f = fx(ArrangementDoc::single(tid(1)));
        let mut ctx = f.ctx();
        let panes = HashMap::new();

        let mut action = bare_action("popup");
        action
            .args
            .insert("command".to_owned(), "lazygit".to_owned().into());
        let effects = run_action(&action, &mut ctx, None, &panes);
        let (_, title, frame) = effects.spawn_floating.expect("a floating spawn");
        assert_eq!(title, "lazygit");
        let FrameKind::SpawnResource {
            command, resource, ..
        } = frame
        else {
            panic!("not a spawn: {frame:?}");
        };
        assert_eq!(resource.and_then(|r| r.retain_secs), Some(0), "retained");
        assert_eq!(
            command,
            Some(vec!["/bin/sh".into(), "-c".into(), "lazygit".into()])
        );

        let before = ctx.overlays.depth();
        let effects = run_action(&bare_action("popup"), &mut ctx, None, &panes);
        assert!(effects.spawn_floating.is_none());
        assert_eq!(ctx.overlays.depth(), before + 1);
    }
}
