//! Only read an existing tmux server. Never source config, spawn a server,
//! execute a binding, or feed imported text to a shell.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug)]
pub(super) struct Snapshot {
    pub session: String,
    pub window: String,
    pub keys: String,
}

pub(super) async fn read(socket: Option<&Path>) -> Result<Snapshot, String> {
    Ok(Snapshot {
        session: query(socket, &["show-options", "-g"]).await?,
        window: query(socket, &["show-options", "-gw"]).await?,
        keys: query(socket, &["list-keys"]).await?,
    })
}

async fn query(socket: Option<&Path>, args: &[&str]) -> Result<String, String> {
    const LIMIT: u64 = 4 * 1024 * 1024;
    let mut command = Command::new("tmux");
    // These read commands do not start a server, including on older tmux.
    if let Some(socket) = socket {
        command.arg("-S").arg(socket);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| format!("cannot run tmux: {err}"))?;
    let stdout = child.stdout.take().ok_or("tmux stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("tmux stderr is unavailable")?;
    let collect = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut stdout = stdout.take(LIMIT + 1);
        let mut stderr = stderr.take(LIMIT + 1);
        let (out_result, err_result) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err),);
        out_result.map_err(|e| e.to_string())?;
        err_result.map_err(|e| e.to_string())?;
        if out.len() as u64 > LIMIT || err.len() as u64 > LIMIT {
            return Err("tmux output exceeded 4 MiB".to_owned());
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!(
                "tmux {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&err).trim().escape_debug()
            ));
        }
        String::from_utf8(out).map_err(|e| format!("tmux output is not UTF-8: {e}"))
    };
    tokio::time::timeout(Duration::from_secs(5), collect)
        .await
        .map_err(|_| "tmux inspection timed out after 5 seconds".to_owned())?
}
