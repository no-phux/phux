//! `defaults.cwd-inheritance = "fixed"`: the seed session, a session an
//! attach creates without a directory, and a new pane all start in
//! `fixed-cwd`.

use phux_config::CwdInheritance;
use phux_server::LaunchCommand;
use tempfile::TempDir;

use crate::common::{Server, attach, attach_create, output_containing, release, sh, spawned};

#[test]
fn fixed_cwd_places_the_seed_an_attach_created_session_and_a_new_pane() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let fixed = tmp.path().canonicalize().unwrap();
        let fixed = fixed.to_str().unwrap().to_owned();
        let mut seed = LaunchCommand::new("/bin/sh");
        seed.args(["-c", "pwd; read _"]);
        let server = Server::builder()
            .session("main")
            .seed(seed)
            .configure(|cfg| {
                cfg.cwd_inheritance = CwdInheritance::Fixed;
                cfg.fixed_cwd = Some(fixed.clone());
            })
            .start();
        let mut stream = server.connect().await;
        let seed = attach(&mut stream, "main").await.focused_resource;
        output_containing(&mut stream, &seed, fixed.as_bytes()).await;

        drop(stream);
        let mut stream = server.connect().await;
        let argv = ["/bin/sh", "-c", "pwd; read _"].map(str::to_owned).to_vec();
        let made = attach_create(&mut stream, "home", Some(argv), None).await;
        output_containing(&mut stream, &made.focused_resource, fixed.as_bytes()).await;

        let pane = spawned(&mut stream, 1, sh("read _; pwd; read _")).await;
        release(&mut stream, &pane).await;
        output_containing(&mut stream, &pane, fixed.as_bytes()).await;
        drop(stream);
        server.stop().await;
    });
}
