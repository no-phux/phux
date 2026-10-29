use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_protocol::input::paste::PasteTrust;
use phux_server::runtime::default_socket_path;

use crate::commands::send_keys::{report_input_failure, report_receipt};
use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target_for_input};

/// `phux paste TARGET [TEXT]` — paste `TEXT` (or all of stdin) into one pane
/// as a single `InputEvent::Paste` over `ROUTE_INPUT`, without attaching; the
/// server picks bracketed or raw delivery from DEC mode 2004. Trusted by
/// default like `send-keys`; `--untrusted` opts into the server's paste gate.
pub(crate) fn run_paste(
    target: &str,
    text: Option<String>,
    untrusted: bool,
    json: bool,
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
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::IO,
                    format!("cannot read paste payload from stdin: {err}"),
                    "pass the text as an argument, or pipe it on stdin",
                ),
                crate::exit_codes::EXIT_FAILURE,
            );
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
        let pane = match resolve_target_for_input(&socket_path, &selector, "paste", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        // One event: it either reached the pane or was refused whole.
        match phux_client::send_keys::paste_to(&socket_path, pane.clone(), data, trust).await {
            Ok(()) => report_receipt(json, &pane, 1),
            Err(err) => report_input_failure(json, target, "paste", &socket_path, 0, &err),
        }
    })
}

/// The paste payload: `TEXT`, else every raw byte of stdin (not necessarily
/// UTF-8, trailing newlines included).
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
