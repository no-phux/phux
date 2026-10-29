//! Session CLI and private export files. The socket and `phux` binary come from [`super::AppOptions`].

use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const MISSING_CLI: &str =
    "Install the phux CLI from https://phux.sh/install so the desktop can change sessions.";

/// The existing project CLI is the only session/recipe builder. Desktop uses
/// its result to attach without re-implementing trust or launch behavior.
pub struct OpenedProject {
    pub session: String,
    pub terminal: phux_protocol::ResourceId,
    pub path: PathBuf,
}

pub fn open_project(phux: &Path, socket: &Path, path: &Path) -> Result<OpenedProject, String> {
    if phux.as_os_str().is_empty() {
        return Err(MISSING_CLI.into());
    }
    let path = crate::platform::coordinator_path(path)?;
    let path = path
        .to_str()
        .ok_or_else(|| "The selected folder has a non-UTF-8 path.".to_owned())?;
    let output = run_output(
        phux,
        socket_args(
            socket,
            &["project", "open", "--prefer-native", "--json", path],
        ),
    )
    .map_err(project_error)?;
    parse_opened_project(&output)
}

/// `phux worktree new`: a linked worktree of `repo` on `branch` (created at
/// `from` when it does not exist yet) and its session, with the CLI's hooks.
pub fn new_worktree(
    phux: &Path,
    socket: &Path,
    repo: &Path,
    branch: &str,
    from: &str,
) -> Result<OpenedProject, String> {
    let repo = repo
        .to_str()
        .ok_or_else(|| "The checkout has a non-UTF-8 path.".to_owned())?;
    if branch.starts_with('-') || from.starts_with('-') {
        return Err(format!("{branch} is not a branch name the CLI can take."));
    }
    let output = run_output(
        phux,
        socket_args(
            socket,
            &[
                "worktree", "new", branch, "--from", from, "--repo", repo, "--json",
            ],
        ),
    )
    .map_err(project_error)?;
    parse_opened_project(&output)
}

fn parse_opened_project(output: &[u8]) -> Result<OpenedProject, String> {
    let result: serde_json::Value = serde_json::from_slice(output)
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
    let root = result
        .get("path")
        .and_then(serde_json::Value::as_str)
        .filter(|path| crate::platform::coordinator_absolute(path))
        .ok_or_else(|| "Project open returned no checkout path.".to_owned())?;
    Ok(OpenedProject {
        session: session.to_owned(),
        terminal: phux_protocol::ResourceId::local(terminal),
        path: PathBuf::from(root),
    })
}

/// The stable code of the CLI's JSON error on the last line of `error`.
fn error_code(error: &str) -> Option<String> {
    let line = error.lines().rfind(|line| !line.trim().is_empty())?;
    let document: serde_json::Value = serde_json::from_str(line).ok()?;
    Some(document.pointer("/error/code")?.as_str()?.to_owned())
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

/// Start only the server. Reconnecting must never create a missing session.
/// A demo supervisor is not restarted.
pub fn start_server(phux: &Path, socket: &Path) -> Result<PathBuf, String> {
    if std::env::var_os("PHUX_DESKTOP_DEMO").is_some() {
        return Err(
            "The private demo server stopped. Relaunch the demo to start a new sandbox.".into(),
        );
    }
    let phux = crate::launch::command_path(phux).map_err(|error| format!("{error:#}"))?;
    crate::launch::ensure_server(&phux, (!socket.as_os_str().is_empty()).then_some(socket))
        .map_err(|error| format!("{error:#}"))
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
    run(phux, socket_args(socket, &["kill", name.trim()])).err()
}

mod add;
mod tailnet;

pub use add::{AddHost, add_host};
pub use tailnet::{Tailnet, TailnetPeer, discover_tailnet};

/// A machine `phux host add` registered for `phux attach NAME`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    pub name: String,
    /// The route a dial takes: `quic://HOST:PORT`, `wss://...`, or `ssh://...`.
    pub endpoint: String,
}

/// The remote registry, from `phux host ls`. Satellites are a hub's peers, not
/// machines this app attaches to, so they are left out.
pub fn list_hosts(phux: &Path) -> Result<Vec<RemoteHost>, String> {
    let output = run_output(phux, args(&["host", "ls", "--role", "remote", "--json"]))?;
    parse_hosts(&output)
}

fn parse_hosts(output: &[u8]) -> Result<Vec<RemoteHost>, String> {
    let document: serde_json::Value = serde_json::from_slice(output)
        .map_err(|_| "Host list returned an invalid response.".to_owned())?;
    let hosts = document
        .get("hosts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Host list returned no hosts.".to_owned())?;
    let mut hosts: Vec<RemoteHost> = hosts
        .iter()
        .filter(|host| host.get("role").and_then(serde_json::Value::as_str) != Some("satellite"))
        .filter_map(|host| {
            Some(RemoteHost {
                name: host.get("name")?.as_str()?.to_owned(),
                endpoint: host
                    .get("endpoint")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .collect();
    hosts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(hosts)
}

/// `phux host rm NAME`. The token file stays, as the CLI leaves it.
pub fn remove_host(phux: &Path, name: &str) -> Result<(), String> {
    run(phux, args(&["host", "rm", "--role", "remote", name]))
}

/// A fresh private file under the temporary directory. The path is never reused.
pub fn write_temp_file(name: &str, text: &str) -> Result<PathBuf, String> {
    // Windows' per-user LocalAppData inherits the user's ACL, unlike a shared
    // temporary directory configured by a service or system account.
    let temporary = if cfg!(windows) {
        crate::platform::state_dir().join("exports")
    } else {
        std::env::temp_dir()
    };
    let base = temporary.join(format!("phux-{}", std::process::id()));
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    let dir = unique_dir(&base)?;
    let file_name = Path::new(name)
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("export.txt"));
    let path = dir.join(file_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path).map_err(|error| error.to_string())?;
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

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
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
    if phux.as_os_str().is_empty() {
        return Err(MISSING_CLI.into());
    }
    let phux = crate::launch::command_path(phux).map_err(|error| format!("{error:#}"))?;
    let output = crate::platform::phux_command(&phux)
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

#[cfg(test)]
mod tests {
    use super::{RemoteHost, parse_hosts, parse_opened_project, project_error};

    #[test]
    fn project_response_uses_coordinator_paths() {
        let opened =
            parse_opened_project(br#"{"session":"work","terminal_id":7,"path":"/home/user/work"}"#)
                .unwrap();
        assert_eq!(opened.path.to_string_lossy(), "/home/user/work");
        assert!(
            parse_opened_project(br#"{"session":"work","terminal_id":7,"path":"relative"}"#)
                .is_err()
        );
        assert!(parse_opened_project(br#"{"session":"work","path":"/home/user/work"}"#).is_err());
    }

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

    #[test]
    fn host_list_keeps_remotes_sorted() {
        let json = br#"{"hosts":[
            {"name":"mini","role":"remote","endpoint":"quic://10.0.0.2:8788"},
            {"name":"hub-peer","role":"satellite","endpoint":"quic://10.0.0.3:8788"},
            {"name":"box","role":"remote","endpoint":"ssh://box"}
        ],"schema_version":1}"#;
        assert_eq!(
            parse_hosts(json).unwrap(),
            vec![
                RemoteHost {
                    name: "box".into(),
                    endpoint: "ssh://box".into()
                },
                RemoteHost {
                    name: "mini".into(),
                    endpoint: "quic://10.0.0.2:8788".into()
                },
            ]
        );
        assert!(parse_hosts(b"not json").is_err());
    }
}
