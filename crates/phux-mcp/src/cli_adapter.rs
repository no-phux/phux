//! Bounded, shell-free adapter to the canonical `phux` CLI JSON surface.
//!
//! MCP parity tools execute the sibling `phux` binary directly with argv —
//! never through a shell — and parse the CLI's versioned JSON. Child lifetime
//! is bounded, cancellation kills the child, and stdout/stderr are drained with
//! fixed memory caps so a broken command cannot exhaust the MCP host.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent at the adapter boundary"
)]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::tools::ToolError;
#[cfg(test)]
use crate::tools::strict_object;

const STDOUT_LIMIT: usize = 1024 * 1024;
const STDERR_LIMIT: usize = 64 * 1024;
pub(crate) const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(crate) struct CliAdapter {
    program: OsString,
}

#[derive(Debug)]
pub(crate) struct CliOutput {
    pub(crate) stdout: String,
    /// The child's stderr, kept for the [`Self::stdout`]-is-empty case under
    /// an *allowed* non-zero exit. The failure path below already turns
    /// stderr into the error message; a verb whose non-zero exit is
    /// sometimes a result and sometimes a failure (`phux status`, `phux
    /// doctor`) has to make that call itself, and it needs the same line to
    /// do it with.
    pub(crate) stderr: String,
}

impl CliAdapter {
    pub(crate) fn discover() -> Self {
        let program = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("phux")))
            .filter(|path| path.is_file())
            .map_or_else(|| OsString::from("phux"), PathBuf::into_os_string);
        Self { program }
    }

    #[cfg(test)]
    pub(crate) fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub(crate) async fn run_json<I, S>(
        &self,
        args: I,
        timeout: Duration,
    ) -> Result<Value, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args, timeout).await?;
        serde_json::from_str(&output.stdout).map_err(|err| {
            ToolError::new(format!(
                "phux returned malformed JSON: {err}; stdout={:?}",
                output.stdout
            ))
        })
    }

    /// Like [`Self::run_json`], but a nonzero child exit is treated as data
    /// rather than an adapter failure.
    ///
    /// `phux run --json` prints the whole `RunResult` and *then* mirrors the
    /// command's exit code (`crates/phux/src/commands/run.rs`), so for that
    /// one verb a nonzero status is the reported result, not a failure to
    /// report one. Every other verb's nonzero exit is a genuine failure,
    /// which is why this is opt-in rather than the default in [`Self::run`].
    /// [`Self::run_allowing`] cannot cover it either: the mirrored codes are
    /// the *command's*, so no allow-list can name them up front.
    ///
    /// A nonzero exit carrying no parseable document still surfaces the
    /// stderr prose, so a real failure (no server, refused target, `run`'s
    /// own timeout) reads exactly as it does through [`Self::run`].
    pub(crate) async fn run_json_mirrored_exit<I, S>(
        &self,
        args: I,
        timeout: Duration,
    ) -> Result<Value, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let (mut child, stdout, stderr) = self.spawn_capturing(args)?;
        let (status, stdout, stderr) = drain_within(&mut child, stdout, stderr, timeout).await?;
        let output = CliOutput {
            stdout: decode_bounded(&stdout, "stdout", STDOUT_LIMIT)?,
            stderr: decode_bounded(&stderr, "stderr", STDERR_LIMIT)?,
        };
        serde_json::from_str(&output.stdout).map_err(|err| {
            // A document parsed: the exit code is data. Nothing parsed: fall
            // back to the failure `run` would have reported.
            check_exit(status, &output.stderr, &[])
                .err()
                .unwrap_or_else(|| {
                    ToolError::new(format!(
                        "phux returned malformed JSON: {err}; stdout={:?}",
                        output.stdout
                    ))
                })
        })
    }

    pub(crate) async fn run<I, S>(&self, args: I, timeout: Duration) -> Result<CliOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_allowing(args, timeout, &[]).await
    }

    /// Like [`Self::run`], but treats each code in `allowed` as a successful
    /// completion rather than a failure.
    ///
    /// This exists for one shape of verb: the ones whose *interesting*
    /// answer rides out on stdout under a non-zero exit. `phux agent wait`
    /// prints its whole result document — baseline, observed edge, the
    /// detector's evidence — and then exits `124` when no transition was
    /// observed. Treating that as a bare failure would throw the document
    /// away and hand the caller an error string, which is precisely the
    /// reading ADR-0076 warns against: a timeout there is not "the tool
    /// broke", it is "no transition happened", and the difference is legible
    /// only in the document.
    pub(crate) async fn run_allowing<I, S>(
        &self,
        args: I,
        timeout: Duration,
        allowed: &[i32],
    ) -> Result<CliOutput, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let (mut child, stdout, stderr) = self.spawn_capturing(args)?;
        let (status, stdout, stderr) = drain_within(&mut child, stdout, stderr, timeout).await?;
        let output = CliOutput {
            stdout: decode_bounded(&stdout, "stdout", STDOUT_LIMIT)?,
            stderr: decode_bounded(&stderr, "stderr", STDERR_LIMIT)?,
        };
        check_exit(status, &output.stderr, allowed)?;
        Ok(output)
    }

    /// Spawn the CLI with argv only — never through a shell — and take both
    /// pipes so the caller can drain them under its own deadline.
    fn spawn_capturing<I, S>(&self, args: I) -> Result<CapturedChild, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| ToolError::new(format!("could not execute phux CLI: {err}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ToolError::new("could not capture phux stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ToolError::new("could not capture phux stderr"))?;
        Ok((child, stdout, stderr))
    }
}

/// A spawned CLI child together with the two pipes taken from it.
type CapturedChild = (Child, ChildStdout, ChildStderr);

/// Wait for the child while draining both pipes, killing it if `timeout`
/// elapses first. Child lifetime is bounded here and nowhere else.
async fn drain_within(
    child: &mut Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    timeout: Duration,
) -> Result<(ExitStatus, BoundedBytes, BoundedBytes), ToolError> {
    let execution = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_bounded(stdout, STDOUT_LIMIT),
            read_bounded(stderr, STDERR_LIMIT),
        );
        Ok::<_, std::io::Error>((status?, stdout?, stderr?))
    };
    let result = Box::pin(tokio::time::timeout(timeout, execution)).await;
    if let Ok(result) = result {
        result.map_err(|err| ToolError::new(format!("phux CLI I/O failed: {err}")))
    } else {
        let _ = child.kill().await;
        Err(ToolError::new(format!(
            "phux CLI exceeded the {}s tool deadline",
            timeout.as_secs_f64()
        )))
    }
}

/// Decode one drained stream, rejecting it when the fixed memory cap clipped
/// it — a truncated document is not a document.
fn decode_bounded(drained: &BoundedBytes, stream: &str, limit: usize) -> Result<String, ToolError> {
    if drained.truncated {
        return Err(ToolError::new(format!(
            "phux CLI {stream} exceeded {limit} bytes"
        )));
    }
    Ok(String::from_utf8_lossy(&drained.bytes).into_owned())
}

/// A non-zero exit is a failure unless the caller allowed that exact code;
/// the child's own stderr line becomes the error message when it wrote one.
fn check_exit(status: ExitStatus, stderr: &str, allowed: &[i32]) -> Result<(), ToolError> {
    if status.success() || status.code().is_some_and(|code| allowed.contains(&code)) {
        return Ok(());
    }
    let message = stderr.trim();
    Err(ToolError::new(if message.is_empty() {
        format!("phux CLI exited with {status}")
    } else {
        message.to_owned()
    }))
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<BoundedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(8192));
    let mut truncated = false;
    let mut chunk = Box::new([0u8; 8192]);
    loop {
        let read = reader.read(chunk.as_mut()).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let keep = remaining.min(read);
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedBytes { bytes, truncated })
}

pub(crate) fn push_socket(argv: &mut Vec<String>, args: &Value) -> Result<(), ToolError> {
    if let Some(socket) = args.get("socket") {
        let socket = socket
            .as_str()
            .ok_or_else(|| ToolError::new("`socket` must be a string"))?;
        argv.push("--socket".to_owned());
        argv.push(socket.to_owned());
    }
    Ok(())
}

pub(crate) fn bounded_string(
    args: &Value,
    key: &str,
    required: bool,
) -> Result<Option<String>, ToolError> {
    let Some(value) = args.get(key) else {
        return if required {
            Err(ToolError::new(format!("missing required string `{key}`")))
        } else {
            Ok(None)
        };
    };
    let value = value
        .as_str()
        .ok_or_else(|| ToolError::new(format!("`{key}` must be a string")))?;
    if value.is_empty() || value.len() > 4096 {
        return Err(ToolError::new(format!(
            "`{key}` must contain 1..=4096 bytes"
        )));
    }
    Ok(Some(value.to_owned()))
}

pub(crate) fn bounded_strings(
    args: &Value,
    key: &str,
    required: bool,
) -> Result<Vec<String>, ToolError> {
    let Some(value) = args.get(key) else {
        return if required {
            Err(ToolError::new(format!("missing required array `{key}`")))
        } else {
            Ok(Vec::new())
        };
    };
    let values = value
        .as_array()
        .ok_or_else(|| ToolError::new(format!("`{key}` must be an array")))?;
    if values.len() > 64 || (required && values.is_empty()) {
        return Err(ToolError::new(format!(
            "`{key}` must contain {}..=64 strings",
            usize::from(required)
        )));
    }
    values
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| ToolError::new(format!("`{key}` must contain only strings")))?;
            if value.is_empty() || value.len() > 4096 {
                return Err(ToolError::new(format!(
                    "each `{key}` entry must contain 1..=4096 bytes"
                )));
            }
            Ok(value.to_owned())
        })
        .collect()
}

pub(crate) fn enum_string(
    args: &Value,
    key: &str,
    allowed: &[&str],
    default: Option<&str>,
) -> Result<String, ToolError> {
    let value = match args.get(key) {
        Some(value) => value
            .as_str()
            .ok_or_else(|| ToolError::new(format!("`{key}` must be a string")))?,
        None => {
            default.ok_or_else(|| ToolError::new(format!("missing required string `{key}`")))?
        }
    };
    if !allowed.contains(&value) {
        return Err(ToolError::new(format!(
            "`{key}` must be one of: {}",
            allowed.join(", ")
        )));
    }
    Ok(value.to_owned())
}

pub(crate) fn ratio(args: &Value) -> Result<Option<f64>, ToolError> {
    let Some(value) = args.get("ratio") else {
        return Ok(None);
    };
    let ratio = value
        .as_f64()
        .ok_or_else(|| ToolError::new("`ratio` must be a number"))?;
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(Some(ratio))
    } else {
        Err(ToolError::new(
            "`ratio` must be finite and strictly between 0 and 1",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn adapter_executes_argv_without_a_shell_and_parses_json() {
        let adapter = CliAdapter::new("printf");
        let value = adapter
            .run_json([r#"{"schema_version":1,"ok":true}"#], DEFAULT_CALL_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(value, json!({ "schema_version": 1, "ok": true }));
    }

    #[tokio::test]
    async fn adapter_enforces_output_and_time_bounds() {
        let adapter = CliAdapter::new("dd");
        let err = adapter
            .run(
                ["if=/dev/zero", "bs=1048577", "count=1"],
                DEFAULT_CALL_TIMEOUT,
            )
            .await
            .unwrap_err();
        assert!(err.0.contains("stdout exceeded"));

        let sleeper = CliAdapter::new("sleep");
        assert!(sleeper.run(["1"], Duration::from_millis(10)).await.is_err());
    }

    /// The `agent wait` shape: a non-zero exit whose stdout is the answer.
    /// An allowed code keeps the document and reports the code; the same
    /// code un-allowed is still a failure.
    #[tokio::test]
    async fn an_allowed_nonzero_exit_keeps_stdout_instead_of_becoming_an_error() {
        let adapter = CliAdapter::new("sh");
        let script = r#"printf '{"satisfied":false}'; exit 124"#;
        let output = adapter
            .run_allowing(["-c", script], DEFAULT_CALL_TIMEOUT, &[124])
            .await
            .expect("124 is allowed here");
        assert_eq!(output.stdout, r#"{"satisfied":false}"#);

        assert!(
            adapter
                .run(["-c", script], DEFAULT_CALL_TIMEOUT)
                .await
                .is_err(),
            "an un-allowed non-zero exit is still a failure",
        );
    }

    /// `phux run` prints its `RunResult` and *then* mirrors the command's
    /// exit code, so a failing command is a successful tool call carrying a
    /// nonzero `exit_code`. The adapter must keep that document instead of
    /// replacing it with stderr. Regression for phux-8gv.
    #[tokio::test]
    async fn mirrored_exit_keeps_the_document_a_failing_command_printed() {
        // `sh` only to synthesize "print a document, then exit nonzero" in a
        // single process. The adapter still execs argv directly; nothing in
        // the production path gains a shell.
        let adapter = CliAdapter::new("sh");
        let argv = [
            "-c",
            r#"printf '{"schema_version":1,"exit_code":3}'; exit 3"#,
        ];

        let value = adapter
            .run_json_mirrored_exit(argv, DEFAULT_CALL_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(value, json!({ "schema_version": 1, "exit_code": 3 }));

        // The default path is unchanged: a nonzero exit is still fatal there,
        // which is what every other verb relies on.
        assert!(adapter.run(argv, DEFAULT_CALL_TIMEOUT).await.is_err());
    }

    /// A nonzero exit with nothing parseable on stdout is a genuine failure
    /// (no server, refused target, `run`'s own timeout). It must keep
    /// reporting the stderr prose rather than degrading into a confusing
    /// "malformed JSON" complaint.
    #[tokio::test]
    async fn mirrored_exit_still_reports_stderr_when_there_is_no_document() {
        let adapter = CliAdapter::new("sh");
        let err = adapter
            .run_json_mirrored_exit(
                ["-c", "echo 'phux: no server at /tmp/x' >&2; exit 1"],
                DEFAULT_CALL_TIMEOUT,
            )
            .await
            .unwrap_err();
        assert_eq!(err.0, "phux: no server at /tmp/x");
    }

    #[test]
    fn strict_argument_parsers_reject_wrong_shapes_and_bounds() {
        assert!(strict_object(&json!({ "x": 1 }), &["x"], &["x"]).is_ok());
        assert!(strict_object(&json!({ "extra": 1 }), &[], &[]).is_err());
        assert!(bounded_string(&json!({}), "x", true).is_err());
        assert!(bounded_strings(&json!({ "x": [1] }), "x", true).is_err());
        assert!(ratio(&json!({ "ratio": 0.0 })).is_err());
    }
}
