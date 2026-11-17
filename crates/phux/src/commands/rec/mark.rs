//! `phux rec mark LABEL`: post an asciicast marker to every recording in
//! progress.

use std::process::ExitCode;

use crate::commands::json_err;

/// Longest marker label `phux rec mark` accepts.
const MARK_LABEL_MAX_CHARS: usize = 256;

/// `phux rec mark LABEL`: append one asciicast marker to every live inbox.
pub(super) fn run_mark(label: &str, json: bool) -> ExitCode {
    if label.chars().count() > MARK_LABEL_MAX_CHARS {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                format!("marker label is longer than {MARK_LABEL_MAX_CHARS} characters"),
                "shorten the label; an empty label is a legal marker",
            ),
            2,
        );
    }
    if label.contains(['\n', '\r']) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                "a marker label cannot contain a newline",
                "pass a single-line label",
            ),
            2,
        );
    }
    match phux_record::post_mark(label) {
        Ok(0) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                "no live recording to mark",
                "start a recording with `phux rec` or `phux --rec`, then run `phux rec mark` again",
            ),
            1,
        ),
        Ok(recordings) => {
            if json {
                outln!("{}", mark_json(label, recordings));
            } else {
                let noun = if recordings == 1 {
                    "recording"
                } else {
                    "recordings"
                };
                if label.is_empty() {
                    outln!("phux: marked {recordings} {noun}");
                } else {
                    outln!("phux: marked {recordings} {noun} ({label})");
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::IO,
                format!("could not post a marker: {err}"),
                "check that the mark directory is writable (`PHUX_REC_MARK_DIR` or $XDG_RUNTIME_DIR/phux-rec-marks)",
            ),
            1,
        ),
    }
}

/// The `phux rec mark --json` result. `recordings` is how many inboxes
/// accepted the label.
pub(super) fn mark_json(label: &str, recordings: usize) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "label": label,
        "recordings": recordings,
    })
}
