use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::read::Verb;
use phux_protocol::input::paste::PasteTrust;

use crate::commands::ctx;
use crate::commands::json_err::{Refusal, codes};
use crate::commands::send_keys::{input_failure, report_receipt};
use crate::commands::{parse_target, resolve_target_for_input};

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
    ctx::run_local(socket, json, |ctx| {
        let selector = parse_target(Some(target))?;
        let data = payload_from(text, &mut std::io::stdin().lock()).map_err(|err| {
            Refusal::new(
                codes::IO,
                format!("cannot read paste payload from stdin: {err}"),
                "pass the text as an argument, or pipe it on stdin",
                crate::exit_codes::EXIT_FAILURE,
            )
        })?;
        let trust = if untrusted {
            PasteTrust::Untrusted
        } else {
            PasteTrust::Trusted
        };
        ctx.rt.block_on(async {
            let pane = resolve_target_for_input(ctx.socket(), &selector, Verb::Paste).await?;
            // One event: it either reached the pane or was refused whole.
            phux_client::send_keys::paste_to(ctx.socket(), pane.clone(), data, trust)
                .await
                .map_err(|err| input_failure(ctx, target, "paste", 0, &err))?;
            Ok(report_receipt(json, &pane, 1))
        })
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

/// Run `phux paste` as parsed.
pub(crate) fn run(
    args: crate::commands::PasteArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::PasteArgs {
        target,
        text,
        untrusted,
        json,
    } = args;
    run_paste(&target, text, untrusted, json.json, socket)
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
}
