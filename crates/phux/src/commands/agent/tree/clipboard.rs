//! The host clipboard, for a mirror's producer: a viewer's paste puts text
//! on it, and a viewer's copy reads it back (`DocumentInput::Paste` and
//! `DocumentInput::CopyClipboard`). Text only, through the system's own
//! tools: `pbcopy`/`pbpaste` on macOS, `wl-copy`/`wl-paste` (wl-clipboard)
//! on Wayland. A missing tool is an error the viewer sees, never a silent
//! fall back to typing.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};

use phux_client::attach::connection::Connection;
use phux_client::tree::{self, Intent};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;

/// The commands that set and read the host clipboard.
pub(super) struct Clipboard {
    /// Reads the new text on stdin.
    set: Vec<String>,
    /// Writes the clipboard's text on stdout.
    get: Vec<String>,
    env: Vec<(&'static str, String)>,
    /// Log a paste instead of setting the clipboard, and refuse a copy.
    dry_run: bool,
}

impl Clipboard {
    /// This host's clipboard.
    pub(super) fn host(dry_run: bool) -> Self {
        let argv = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect();
        #[cfg(target_os = "linux")]
        let (set, get, env) = (
            argv(&["wl-copy", "--type", "text/plain;charset=utf-8"]),
            argv(&["wl-paste", "--no-newline", "--type", "text"]),
            super::surface_grim::session_env(),
        );
        #[cfg(not(target_os = "linux"))]
        let (set, get, env) = (argv(&["pbcopy"]), argv(&["pbpaste"]), Vec::new());
        Self {
            set,
            get,
            env,
            dry_run,
        }
    }

    fn command(&self, argv: &[String]) -> Command {
        let mut command = Command::new(&argv[0]);
        command.args(&argv[1..]).envs(self.env.iter().cloned());
        command
    }

    fn spawn_failed(argv: &[String], err: &std::io::Error) -> String {
        if err.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{} is not installed here (wl-clipboard on Wayland)",
                argv[0]
            )
        } else {
            format!("could not run {}: {err}", argv[0])
        }
    }

    /// Put `text` on the host clipboard.
    pub(super) fn set(&self, text: &str) -> Result<(), String> {
        if self.dry_run {
            eprintln!(
                "dry-run: would put {} bytes on the host clipboard",
                text.len()
            );
            return Ok(());
        }
        // `wl-copy` stays behind to serve the selection: its output must not
        // be a pipe this process waits on.
        let mut child = self
            .command(&self.set)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Self::spawn_failed(&self.set, &err))?;
        let written = child
            .stdin
            .take()
            .map_or(Ok(()), |mut stdin| stdin.write_all(text.as_bytes()));
        let status = child.wait().map_err(|err| err.to_string())?;
        written.map_err(|err| format!("{} did not take the text: {err}", self.set[0]))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} failed ({status})", self.set[0]))
        }
    }

    /// The host clipboard's text, at most [`MAX_CLIPBOARD_TEXT`] bytes.
    pub(super) fn get(&self) -> Result<String, String> {
        if self.dry_run {
            return Err("dry run: the host clipboard is not read".to_owned());
        }
        let mut child = self
            .command(&self.get)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| Self::spawn_failed(&self.get, &err))?;
        let mut bytes = Vec::new();
        let cap = MAX_CLIPBOARD_TEXT as u64 + 1;
        let read = child
            .stdout
            .take()
            .map_or(Ok(0), |out| out.take(cap).read_to_end(&mut bytes));
        if bytes.len() > MAX_CLIPBOARD_TEXT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the host clipboard holds more than 1 MiB of text".to_owned());
        }
        let status = child.wait().map_err(|err| err.to_string())?;
        read.map_err(|err| format!("could not read {}: {err}", self.get[0]))?;
        if !status.success() {
            return Err(format!(
                "{} failed ({status}); the clipboard may hold no text",
                self.get[0]
            ));
        }
        String::from_utf8(bytes).map_err(|_| "the host clipboard holds no text".to_owned())
    }

    /// Answer copy `reply` on `resource` with the clipboard's text. A
    /// failure is logged; the mirror goes on.
    #[allow(
        clippy::future_not_send,
        reason = "the CLI runtime is single-threaded; the connection is not shared"
    )]
    pub(super) async fn answer(
        &self,
        conn: &mut Connection,
        resource: &ResourceId,
        reply: u32,
        intents: &mut Vec<Intent>,
    ) {
        let text = self.get();
        if let Err(message) = &text {
            eprintln!("phux: copy from the host clipboard failed: {message}");
        }
        if let Err(err) = tree::answer_copy(conn, resource, reply, text, intents).await {
            eprintln!("phux: copy not answered: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clipboard that is a file, through `sh` and `cat`.
    fn fake(dir: &std::path::Path, get: &str) -> Clipboard {
        let board = dir.join("board");
        let sh = |script: String| vec!["sh".to_owned(), "-c".to_owned(), script];
        Clipboard {
            set: sh(format!("cat > '{}'", board.display())),
            get: sh(get.replace("BOARD", &board.display().to_string())),
            env: Vec::new(),
            dry_run: false,
        }
    }

    #[test]
    fn a_paste_lands_on_the_board_and_a_copy_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let board = fake(dir.path(), "cat 'BOARD'");
        board.set("héllo\nworld").unwrap();
        assert_eq!(board.get().unwrap(), "héllo\nworld");
    }

    #[test]
    fn a_copy_over_the_cap_or_of_no_text_or_a_failing_tool_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let over = fake(
            dir.path(),
            &format!("head -c {} /dev/zero", MAX_CLIPBOARD_TEXT + 1),
        );
        assert!(over.get().unwrap_err().contains("1 MiB"));
        let binary = fake(dir.path(), r"printf '\377\376'");
        assert!(binary.get().unwrap_err().contains("no text"));
        let failing = fake(dir.path(), "exit 1");
        assert!(failing.get().unwrap_err().contains("failed"));
        let at_cap = fake(
            dir.path(),
            &format!("head -c {MAX_CLIPBOARD_TEXT} /dev/zero"),
        );
        assert_eq!(at_cap.get().unwrap().len(), MAX_CLIPBOARD_TEXT);
    }

    #[test]
    fn a_missing_tool_names_itself_and_a_dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut missing = fake(dir.path(), "true");
        missing.get = vec!["phux-no-such-clipboard-tool".to_owned()];
        assert!(
            missing
                .get()
                .unwrap_err()
                .contains("phux-no-such-clipboard-tool is not installed")
        );
        let mut dry = fake(dir.path(), "cat 'BOARD'");
        dry.dry_run = true;
        dry.set("nope").unwrap();
        assert!(!dir.path().join("board").exists());
        assert!(dry.get().is_err());
    }
}
