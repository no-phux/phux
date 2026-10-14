//! Session CLI and private export files. The socket and `phux` binary come from [`super::AppOptions`].

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const MISSING_CLI: &str =
    "Install the phux CLI from https://phux.sh/install so the desktop can change sessions.";

/// The existing project CLI is the only session/recipe builder. Desktop uses
/// its result to attach without re-implementing trust or launch behavior.
pub struct OpenedProject {
    pub session: String,
    pub terminal: phux_protocol::ResourceId,
}

pub fn open_project(phux: &Path, socket: &Path, path: &Path) -> Result<OpenedProject, String> {
    if phux.as_os_str().is_empty() {
        return Err(MISSING_CLI.into());
    }
    let path = path
        .to_str()
        .ok_or_else(|| "The selected folder has a non-UTF-8 path.".to_owned())?;
    let output = run_output(
        phux,
        socket_args(socket, &["project", "open", "--json", path]),
    )
    .map_err(project_error)?;
    let result: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|_| "Project open returned an invalid response.".to_owned())?;
    let session = result
        .get("session")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "Project open returned no session.".to_owned())?;
    let terminal = result
        .get("terminal_id")
        .and_then(serde_json::Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| "Project open returned no local terminal.".to_owned())?;
    Ok(OpenedProject {
        session: session.to_owned(),
        terminal: phux_protocol::ResourceId::local(terminal),
    })
}

/// The CLI's JSON refusal belongs on stderr; show its actionable message,
/// not the wire document, when a folder's recipe needs trust or a name collides.
fn project_error(error: String) -> String {
    let line = error.lines().rfind(|line| !line.trim().is_empty());
    let Some(Ok(document)) = line.map(serde_json::from_str::<serde_json::Value>) else {
        return error;
    };
    let Some(message) = document
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str)
    else {
        return error;
    };
    match document
        .get("remedy")
        .and_then(serde_json::Value::as_str)
        .filter(|remedy| !remedy.is_empty())
    {
        Some(remedy) => format!("{message}\n{remedy}"),
        None => message.to_owned(),
    }
}

/// Start the server the launch used, then name the session to open: `session`
/// (created persistent when missing), or with no session, the Inbox or the
/// most recent session. A demo supervisor is not restarted.
pub fn ensure_server(phux: &Path, socket: &Path, session: &str) -> Result<String, String> {
    if std::env::var_os("PHUX_DESKTOP_DEMO").is_some() {
        return Err(
            "The private demo server stopped. Relaunch the demo to start a new sandbox.".into(),
        );
    }
    if phux.as_os_str().is_empty() {
        return Err(
            "Install the phux CLI from https://phux.sh/install so the desktop can start a server."
                .into(),
        );
    }
    restart(phux, socket)?;
    let requested = Some(session).filter(|name| !name.trim().is_empty());
    crate::connection::home::open_home(phux, socket, requested)
}

fn restart(phux: &Path, socket: &Path) -> Result<(), String> {
    let explicit = std::env::var("PHUX_SOCKET")
        .ok()
        .filter(|value| !value.is_empty());
    let ensure_socket = explicit.as_deref().map(Path::new).unwrap_or(socket);
    run(phux, socket_args(ensure_socket, &["server", "--ensure"]))?;
    let listening = if explicit.is_some() {
        ensure_socket.to_path_buf()
    } else {
        server_socket(phux)?
    };
    if listening != socket {
        return Err(format!(
            "The server listens on {}, not {}. Relaunch Phux to use it.",
            listening.display(),
            socket.display()
        ));
    }
    Ok(())
}

/// `phux new` for a server without `ORGANIZATION`; the CLI makes a session it
/// is given a name for persistent.
pub fn create_session(phux: &Path, socket: &Path, name: &str, directory: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("A session needs a name.".into());
    }
    if directory.trim().is_empty() {
        return Some("A new session needs the focused directory or a chosen folder.".into());
    }
    if phux.as_os_str().is_empty() {
        return Some(MISSING_CLI.into());
    }
    run(
        phux,
        socket_args(
            socket,
            &[
                "new",
                "--json",
                "-s",
                name.trim(),
                "--cwd",
                directory.trim(),
            ],
        ),
    )
    .err()
}

pub fn rename_session(phux: &Path, socket: &Path, current: &str, next: &str) -> Option<String> {
    if current.trim().is_empty() || next.trim().is_empty() {
        return Some("Rename needs the current session and a new name.".into());
    }
    if current.trim() == next.trim() {
        return None;
    }
    if phux.as_os_str().is_empty() {
        return Some(MISSING_CLI.into());
    }
    run(
        phux,
        socket_args(socket, &["rename", current.trim(), next.trim()]),
    )
    .err()
}

/// `phux kill` for a server without `SESSION_KILL`.
pub fn kill_session(phux: &Path, socket: &Path, name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("Kill needs a session name.".into());
    }
    if phux.as_os_str().is_empty() {
        return Some(MISSING_CLI.into());
    }
    run(phux, socket_args(socket, &["kill", name.trim()])).err()
}

/// A fresh private file under the temporary directory. The path is never reused.
pub fn write_temp_file(name: &str, text: &str) -> Result<PathBuf, String> {
    let base = std::env::temp_dir().join(format!("phux-{}", std::process::id()));
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    let dir = unique_dir(&base)?;
    let file_name = Path::new(name)
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("export.txt"));
    let path = dir.join(file_name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| error.to_string())?;
    file.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(path)
}

fn unique_dir(base: &Path) -> Result<PathBuf, String> {
    for n in 0..64 {
        let path = base.join(format!(
            "{n}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Could not create a private temporary directory.".into())
}

fn socket_args(socket: &Path, rest: &[&str]) -> Vec<String> {
    let mut args = vec!["--socket".into(), socket.display().to_string()];
    args.extend(rest.iter().map(|part| (*part).to_owned()));
    args
}

fn run(phux: &Path, args: Vec<String>) -> Result<(), String> {
    run_output(phux, args).map(|_| ())
}

fn run_output(phux: &Path, args: Vec<String>) -> Result<Vec<u8>, String> {
    let output = Command::new(phux)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("Could not run {}: {error}", phux.display()))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if !stderr.trim().is_empty() {
        stderr.trim()
    } else {
        stdout.trim()
    };
    Err(if detail.is_empty() {
        format!("{} exited {}", phux.display(), output.status)
    } else {
        detail.to_owned()
    })
}

fn server_socket(phux: &Path) -> Result<PathBuf, String> {
    let output = Command::new(phux)
        .args(["status", "--json"])
        .output()
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(text.trim())
        .map_err(|_| "No running phux server after `phux server --ensure`".to_owned())?;
    value
        .get("socket")
        .and_then(|socket| socket.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| "No running phux server after `phux server --ensure`".to_owned())
}

#[cfg(test)]
mod tests {
    use super::project_error;

    #[test]
    fn project_refusal_shows_remedy_without_json() {
        let json = r#"{"error":{"code":"Workspace","message":"recipe is not trusted"},"remedy":"run `phux project trust`","exit_code":1}"#;
        assert_eq!(
            project_error(json.into()),
            "recipe is not trusted\nrun `phux project trust`"
        );
        assert_eq!(
            project_error(format!("phux: warning: recipe changed\n{json}\n")),
            "recipe is not trusted\nrun `phux project trust`"
        );
        assert_eq!(
            project_error("server unavailable".into()),
            "server unavailable"
        );
    }
}
