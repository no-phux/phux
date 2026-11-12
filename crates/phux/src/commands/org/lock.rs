//! `phux lock TAB` / `phux unlock TAB`: lock a tab against close.

use std::process::ExitCode;

use phux_client::layout_ops::LayoutOps;
use phux_client::org;
use phux_client::selector::{self, GroupRef, Selector};

use super::{connect, finish, records};
use crate::commands::json_err::{Refusal, codes};
use crate::commands::server_target::ServerSpec;

/// Lock (`locked`) or unlock the tab `target` names (`SESSION:N` or
/// `SESSION:NAME`). A locked tab refuses every close, from any client,
/// until it is unlocked; killing its whole session still ends it.
pub(crate) fn run_lock(target: &str, locked: bool, json: bool, server: ServerSpec) -> ExitCode {
    let verb = if locked { "lock" } else { "unlock" };
    let Ok(Selector::Group(session, group)) = selector::parse(target) else {
        return Refusal::new(
            codes::INVALID_SELECTOR,
            format!("{verb}: '{target}' names no tab"),
            "name a tab as SESSION:N or SESSION:NAME (`phux help targets`)",
            2,
        )
        .emit(json);
    };
    let (rt, server) = match server.prepare(verb, json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let missing = || {
        Refusal::new(
            codes::NO_SUCH_TARGET,
            format!("{verb}: no tab {target}"),
            "run `phux ls` to see sessions and their tabs",
            2,
        )
    };
    rt.block_on(async {
        let mut conn = match connect(&server, json, verb).await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let inventory = match records(&mut conn, verb).await {
            Ok(inventory) => inventory,
            Err(failure) => return failure.emit(json),
        };
        let Some(live) = inventory.session_named(&session).and_then(|s| s.live) else {
            return missing().emit(json);
        };
        let Ok(read) = LayoutOps::new(&mut conn, live, 0x5100)
            .read_arrangement()
            .await
        else {
            return missing().emit(json);
        };
        let groups = &read.document.groups;
        let found = match &group {
            GroupRef::Index(n) => groups.get(usize::from(*n)),
            GroupRef::Tag(name) => groups.iter().find(|g| g.name == *name),
        };
        let Some(found) = found else {
            return missing().emit(json);
        };
        let result = org::lock_group(&mut conn, read.key, found.id, locked).await;
        drop(conn);
        finish(
            json,
            verb,
            target,
            target,
            result,
            &format!("{verb}ed {target}"),
        )
    })
}
