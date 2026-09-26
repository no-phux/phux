//! Own the server, Bash PTY, home directory, and bounded Bun fixture lifetime.
use std::{path::Path, time::Duration};

use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use phux_server_testkit::{SERVER_JOIN_DEADLINE, run_local, spawn_server_with_seed_cmd};

async fn run_fixture(script: &str, home: &Path, socket: &Path) -> Result<(), String> {
    let mut child = tokio::process::Command::new("bun")
        .arg(script)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("PHUX_FEASIBILITY_HOME", home)
        .env("PHUX_FEASIBILITY_SOCKET", socket)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("start Bun: {error}"))?;
    let pid = Pid::from_raw(i32::try_from(child.id().expect("new child PID")).expect("Unix PID"));
    let result = tokio::time::timeout(Duration::from_secs(80), child.wait()).await;
    // The runner normally reaps its native child. A crashed or wedged runner
    // must not leave that descendant window running after the server stops.
    match killpg(pid, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => (),
        Err(error) => return Err(format!("kill owned fixture process group: {error}")),
    }
    match result {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(result) => Err(format!("Bun fixture failed: {result:?}")),
        Err(_) => {
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .map_err(|_| "Bun did not reap after kill")?
                .map_err(|error| error.to_string())?;
            Err("Bun fixture exceeded its deadline".into())
        }
    }
}

fn main() {
    let script = std::env::args().nth(1).expect("Bun fixture script");
    run_local(async move {
        let temp = tempfile::TempDir::new().expect("isolated fixture directory");
        let socket = temp.path().join("solid.sock");
        std::fs::write(temp.path().join("editor.txt"), "VIM READY\n").expect("seed Vim file");
        let mut command = portable_pty::CommandBuilder::new("/bin/bash");
        command.args(["--noprofile", "--norc", "-i"]);
        command.cwd(temp.path());
        command.env("HOME", temp.path());
        command.env("XDG_CONFIG_HOME", temp.path().join("config"));
        command.env("XDG_DATA_HOME", temp.path().join("data"));
        command.env("XDG_CACHE_HOME", temp.path().join("cache"));
        command.env("PS1", "fixture> ");
        command.env("TERM", "xterm-256color");
        command.env("LC_ALL", "en_US.UTF-8");
        let (shutdown, server) =
            spawn_server_with_seed_cmd(socket.clone(), "solid-feasibility", command);
        let result = run_fixture(&script, temp.path(), &socket).await;
        let shutdown_result = shutdown.send(());
        let joined = tokio::time::timeout(SERVER_JOIN_DEADLINE, server).await;
        shutdown_result.expect("server still alive");
        joined
            .expect("server stop deadline")
            .expect("server task")
            .expect("server shutdown");
        result.expect("production Solid terminal feasibility");
    });
}
