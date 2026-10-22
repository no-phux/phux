//! Who may feed an `AgentSession` (phux-jqlh0.8): any local connection, not
//! only the one that spawned the session. `phux agent session open` and each
//! `phux agent emit` are separate processes on separate connections, so a
//! producer bound to the opener would make the documented flow impossible.

use phux_protocol::wire::frame::{Command, CommandResult, SpawnResource, SpawnResult};
use phux_server_testkit::{Spawn, command, spawn_resource};
use portable_pty::CommandBuilder;

use crate::common::{Server, attach, sh, spawned};

const FOREVER: &str = "while :; do sleep 3600; done";

#[test]
fn a_local_connection_that_did_not_open_the_session_may_append() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut opener = server.connect().await;
        attach(&mut opener, "demo").await;
        let parent = spawned(&mut opener, 1, sh(FOREVER)).await;
        let spawn = Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(parent, "mytool"))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(session) = spawn_resource(&mut opener, 2, spawn).await else {
            panic!("spawn session failed");
        };
        // The opener goes away, as `phux agent session open` does.
        drop(opener);

        let mut emitter = server.connect().await;
        let result = command(
            &mut emitter,
            1,
            Command::AppendResourceOutput {
                terminal_id: session,
                bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
            },
        )
        .await;
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "a fresh local connection must be able to append: {result:?}"
        );
        server.stop().await;
    });
}
