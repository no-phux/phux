//! Session CLI and private export files. The socket and `phux` binary come from [`super::AppOptions`].

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const MISSING_CLI: &str =
    "Install the phux CLI from https://phux.sh/install so the desktop can change sessions.";

/// Start the server the launch used, then the home session. A demo supervisor is not restarted.
pub fn ensure_server(phux: &Path, socket: &Path, session: &str) -> Option<String> {
    if std::env::var_os("PHUX_DESKTOP_DEMO").is_some() {
        return Some(
            "The private demo server stopped. Relaunch the demo to start a new sandbox.".into(),
        );
    }
    if phux.as_os_str().is_empty() {
        return Some(
            "Install the phux CLI from https://phux.sh/install so the desktop can start a server."
                .into(),
        );
    }
    match restart(phux, socket, session) {
        Ok(()) => None,
        Err(error) => Some(error),
    }
}

fn restart(phux: &Path, socket: &Path, session: &str) -> Result<(), String> {
    let explicit = std::env::var("PHUX_SOCKET").ok().filter(|value| !value.is_empty());
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
    if !session_names(phux, socket)?.iter().any(|name| name == session) {
        run(phux, socket_args(socket, &["new", "--json", "-s", session]))?;
    }
    Ok(())
}

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
        socket_args(socket, &["new", "--json", "-s", name.trim(), "--cwd", directory.trim()]),
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
    run(phux, socket_args(socket, &["rename", current.trim(), next.trim()])).err()
}

pub fn close_session(phux: &Path, socket: &Path, name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return Some("Close needs a session name.".into());
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
    file.write_all(text.as_bytes()).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(path)
}

fn unique_dir(base: &Path) -> Result<PathBuf, String> {
    for n in 0..64 {
        let path = base.join(format!("{n}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
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
    let output = Command::new(phux)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| format!("Could not run {}: {error}", phux.display()))?;
    if output.status.success() {
        return Ok(());
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

fn session_names(phux: &Path, socket: &Path) -> Result<Vec<String>, String> {
    let output = Command::new(phux)
        .args(socket_args(socket, &["status", "--json"]))
        .output()
        .map_err(|error| error.to_string())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap_or(serde_json::Value::Null);
    let Some(sessions) = value.get("sessions").and_then(|item| item.as_array()) else {
        return Ok(Vec::new());
    };
    Ok(sessions
        .iter()
        .filter_map(|session| {
            session
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_owned)
        })
        .collect())
}

/// Shell-quote dropped paths. A drop is text, never a command.
pub fn quote_dropped_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| {
            let text = path.display().to_string();
            if text.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
            }) {
                text
            } else {
                format!("'{}'", text.replace('\'', r#"'"'"'"#))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_paths_are_one_shell_word_each() {
        let quoted = quote_dropped_paths(&[
            PathBuf::from("/tmp/plain.txt"),
            PathBuf::from("/tmp/has space.txt"),
        ]);
        assert_eq!(quoted, "/tmp/plain.txt '/tmp/has space.txt'");
        assert!(!quoted.contains('\n'));
    }
}
