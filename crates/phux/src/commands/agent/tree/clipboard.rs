//! The host clipboard, for a mirror's producer: a viewer's paste puts text
//! on it, and a viewer's copy reads it back (`DocumentInput::Paste` and
//! `DocumentInput::CopyClipboard`), and a viewer's copy of the selection
//! reads it after the copy chord (`DocumentInput::CopySelection`). Text only, through the system's own
//! tools: `pbcopy`/`pbpaste` on macOS, `wl-copy`/`wl-paste` (wl-clipboard)
//! on Wayland. A missing tool is an error the viewer sees, never a silent
//! fall back to typing.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};

use phux_client::attach::connection::Connection;
use phux_client::tree::{self, DocumentInput, Intent};
use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;

/// What a viewer sees when the host clipboard holds nothing.
pub(super) const NOTHING_COPIED: &str = "Nothing copied on the host";

/// How long after the copy chord the clipboard is read: the focused app
/// sets it asynchronously (a Wayland client hands it to the compositor).
/// ponytail: a fixed wait; poll for a change if slow apps miss it.
const COPY_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

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
        // `wl-paste` fails on an empty clipboard ("Nothing is copied").
        if !status.success() && bytes.is_empty() {
            return Err(format!("{NOTHING_COPIED} ({} {status})", self.get[0]));
        }
        if !status.success() {
            return Err(format!("{} failed ({status})", self.get[0]));
        }
        match String::from_utf8(bytes) {
            Ok(text) if text.is_empty() => Err(NOTHING_COPIED.to_owned()),
            Ok(text) => Ok(text),
            Err(_) => Err("the host clipboard holds no text".to_owned()),
        }
    }

    /// Answer `intent` when it is a copy, with the clipboard's text; false
    /// for any other intent. A failure is logged; the mirror goes on.
    #[allow(
        clippy::future_not_send,
        reason = "the CLI runtime is single-threaded; the connection is not shared"
    )]
    pub(super) async fn answered(
        &self,
        conn: &mut Connection,
        intent: &Intent,
        intents: &mut Vec<Intent>,
    ) -> bool {
        let DocumentInput::CopyClipboard { reply } = intent.input else {
            return false;
        };
        let text = self.get();
        if let Err(message) = &text {
            eprintln!("phux: copy from the host clipboard failed: {message}");
        }
        if let Err(err) = tree::answer_copy(conn, &intent.resource, reply, text, intents).await {
            eprintln!("phux: copy not answered: {err}");
        }
        true
    }

    /// Carry out `intent` on a window or display: a copy is answered, a
    /// paste sets the clipboard before `act` presses the chord, and
    /// anything else goes straight to `act`. A failure is logged, and a
    /// paste's outcome, or a copy of the selection, goes back to the viewer
    /// ([`Self::report`]).
    #[allow(
        clippy::future_not_send,
        reason = "the CLI runtime is single-threaded; the connection is not shared"
    )]
    pub(super) async fn carry_out(
        &self,
        conn: &mut Connection,
        intent: &Intent,
        intents: &mut Vec<Intent>,
        act: impl FnOnce(&DocumentInput) -> Result<(), String>,
    ) {
        if self.answered(conn, intent, intents).await {
            return;
        }
        let outcome = match &intent.input {
            DocumentInput::Paste { text, .. } => self.set(text).and_then(|()| act(&intent.input)),
            input => act(input),
        };
        if let Err(message) = &outcome {
            eprintln!("phux: input skipped: {message}");
        }
        self.report(conn, intent, outcome, intents).await;
    }

    /// Tell the viewer how its paste or copy of the selection went, once
    /// `outcome` says whether its chord was pressed. A copy waits for the
    /// clipboard to settle and answers with its text. A server holding the
    /// paste (`SEND_DOCUMENT_INPUT`) takes a `ClipboardText` answer with
    /// reply 0 as its outcome; an older server refuses it, which is ignored.
    #[allow(
        clippy::future_not_send,
        reason = "the CLI runtime is single-threaded; the connection is not shared"
    )]
    pub(super) async fn report(
        &self,
        conn: &mut Connection,
        intent: &Intent,
        outcome: Result<(), String>,
        intents: &mut Vec<Intent>,
    ) {
        if let DocumentInput::CopySelection { reply, .. } = intent.input {
            if outcome.is_ok() {
                tokio::time::sleep(COPY_SETTLE).await;
            }
            let text = outcome.and_then(|()| self.get());
            if let Err(message) = &text {
                eprintln!("phux: copy of the selection failed: {message}");
            }
            if let Err(err) = tree::answer_copy(conn, &intent.resource, reply, text, intents).await
            {
                eprintln!("phux: copy not answered: {err}");
            }
            return;
        }
        if !matches!(intent.input, DocumentInput::Paste { .. }) {
            return;
        }
        let text = outcome.map(|()| String::new());
        if let Err(err) = tree::answer_copy(conn, &intent.resource, 0, text, intents).await {
            tracing::debug!(%err, "paste outcome not taken");
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
        let failing = fake(dir.path(), "printf x; exit 1");
        assert!(failing.get().unwrap_err().contains("failed"));
        // An empty clipboard says so, whether the tool fails (wl-paste) or
        // prints nothing (pbpaste).
        for empty in ["exit 1", "true"] {
            let empty = fake(dir.path(), empty);
            assert!(empty.get().unwrap_err().starts_with(NOTHING_COPIED));
        }
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
