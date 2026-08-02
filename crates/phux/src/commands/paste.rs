use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_protocol::input::paste::PasteTrust;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, report_no_server, resolve_target};

/// `phux paste TARGET [TEXT]` — paste a payload into a pane via the
/// side-effect-free `ROUTE_INPUT` route.
///
/// `TARGET` is the full selector grammar (`docs/consumers/tui.md` §3),
/// resolved client-side to a single pane exactly like `send-keys`
/// (phux-n95). The payload is `TEXT`, or all of stdin when `TEXT` is
/// omitted; it rides as ONE `InputEvent::Paste`, so the pane is neither
/// attached nor resized and the server picks bracketed vs raw delivery
/// from the pane's DEC mode 2004 state.
///
/// Trust: pastes are trusted by default — the caller vouches for content
/// it composed, the same ungated authority `send-keys` has. `--untrusted`
/// opts into the server-side safety gate, under which the pane's policy
/// (reject by default) may silently drop an unsafe payload.
pub(crate) fn run_paste(
    target: &str,
    text: Option<String>,
    untrusted: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let payload = payload_from(text, &mut std::io::stdin().lock());
    let data = match payload {
        Ok(data) => data,
        Err(err) => {
            eprintln!("phux: cannot read paste payload from stdin: {err}");
            return ExitCode::FAILURE;
        }
    };
    let trust = if untrusted {
        PasteTrust::Untrusted
    } else {
        PasteTrust::Trusted
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "paste", false).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::send_keys::paste_to(&socket_path, pane, data, trust).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err @ AttachError::Io(_)) => report_no_server(&err, &socket_path, "paste"),
            Err(AttachError::Refused(msg)) => {
                eprintln!("phux: cannot paste to '{target}': {msg} (try `phux ls`)");
                ExitCode::FAILURE
            }
            Err(err) => {
                eprintln!("phux: paste failed: {err}");
                ExitCode::FAILURE
            }
        }
    })
}

/// Resolve the paste payload: the `TEXT` argument when given, else every
/// byte of `stdin` (the pipe form: `git diff | phux paste review`).
///
/// The stdin path reads raw bytes, not lines — a paste payload need not
/// be UTF-8, and trailing newlines are part of what the user piped.
fn payload_from(text: Option<String>, stdin: &mut impl Read) -> std::io::Result<Vec<u8>> {
    if let Some(text) = text {
        Ok(text.into_bytes())
    } else {
        let mut buf = Vec::new();
        stdin.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An explicit TEXT argument is the payload verbatim; stdin is not
    /// touched (the reader stays unread).
    #[test]
    fn text_argument_wins_over_stdin() {
        let mut stdin: &[u8] = b"stdin must not be read";
        let payload = payload_from(Some("from the arg".to_owned()), &mut stdin).unwrap();
        assert_eq!(payload, b"from the arg");
        assert_eq!(
            stdin, b"stdin must not be read",
            "TEXT form must not consume stdin",
        );
    }

    /// With TEXT omitted, the payload is all of stdin, raw bytes included
    /// — embedded newlines and a trailing newline survive.
    #[test]
    fn omitted_text_reads_all_of_stdin() {
        let mut stdin: &[u8] = b"line one\nline two\n";
        let payload = payload_from(None, &mut stdin).unwrap();
        assert_eq!(payload, b"line one\nline two\n");
        assert!(stdin.is_empty(), "stdin must be drained");
    }

    /// Non-UTF-8 stdin is a valid payload — paste carries bytes, not text.
    #[test]
    fn stdin_payload_may_be_non_utf8() {
        let mut stdin: &[u8] = &[0xff, 0xfe, b'x'];
        let payload = payload_from(None, &mut stdin).unwrap();
        assert_eq!(payload, vec![0xff, 0xfe, b'x']);
    }
}
