//! A document's label reaches every client as its name: a tree or surface
//! is named for what it observes, a typed document for its `--name`.

use phux_protocol::wire::frame::{SpawnResource, SpawnResult};
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{Spawn, spawn_resource};

use crate::common::{Server, attach, find, sh, spawned, wait_for_state};

const FOREVER: &str = "while :; do sleep 3600; done";

#[test]
fn documents_publish_their_labels_as_names() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let specs = [
            SpawnResource::accessibility_tree(parent.clone(), "Safari"),
            SpawnResource::pixel_surface(parent.clone(), "display"),
            SpawnResource::document(parent.clone(), "text/plain", Some("notes".to_owned())),
        ];
        let mut docs = Vec::new();
        for (request, spec) in (2..).zip(specs) {
            let spawn = Spawn {
                resource: Some(Box::new(spec)),
                ..Spawn::default()
            };
            let SpawnResult::Ok(doc) = spawn_resource(&mut owner, request, spawn).await else {
                panic!("spawn document failed");
            };
            docs.push(doc);
        }

        let mut viewer = server.connect().await;
        let names = |s: &_| -> Vec<Option<String>> {
            docs.iter()
                .map(|doc| find(s, doc).and_then(|info| info.title.clone()))
                .collect()
        };
        let want = ["Safari", "display", "notes"].map(|n| Some(n.to_owned()));
        wait_for_state(&mut viewer, 10, "document names", |s| names(s) == want).await;

        drop(viewer);
        drop(owner);
        server.stop().await;
    });
}
