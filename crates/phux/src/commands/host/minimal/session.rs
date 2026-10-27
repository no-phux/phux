//! A joined box's own session on this hub.
//!
//! When a box joins, `sync` opens session `min-NAME` here with one pane
//! running `min session attach` into the box, so the box shows up in every
//! phux client, the desktop app included, as a session with a shell in it.
//! The pane starts in the directory the box was activated from, so the
//! session sits with that project's. The session is ephemeral: destroying the
//! box ends the attach, and the session ends with its last process.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use super::boxes::{BoxSession, MinCli};

/// Open session `name` on this machine's server, its pane attached to `session`'s box.
pub(super) fn open(min: &MinCli, session: &BoxSession, name: &str) -> Result<(), String> {
    let phux = std::env::current_exe().map_err(|err| format!("could not find phux: {err}"))?;
    let project = session.project.as_deref().filter(|dir| dir.is_dir());
    let output = Command::new(phux)
        .args(new_args(
            min.program(),
            min.search_path(),
            project,
            &session.id,
            name,
        ))
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("could not run `phux new`: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
}

/// `phux new` for the box's session. The pane gets `min`'s `PATH`, which
/// the server's own environment may lack.
pub(super) fn new_args(
    min: &Path,
    path: Option<OsString>,
    project: Option<&Path>,
    box_id: &str,
    name: &str,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["new", "--json", "--ephemeral", "-s", name]
        .into_iter()
        .map(OsString::from)
        .collect();
    if let Some(project) = project {
        args.extend([OsString::from("-c"), project.as_os_str().to_owned()]);
    }
    if let Some(path) = path {
        let mut env = OsString::from("PATH=");
        env.push(path);
        args.extend([OsString::from("-e"), env]);
    }
    args.extend([
        OsString::from("--"),
        min.as_os_str().to_owned(),
        OsString::from("session"),
        OsString::from("attach"),
        OsString::from(box_id),
    ]);
    args
}
