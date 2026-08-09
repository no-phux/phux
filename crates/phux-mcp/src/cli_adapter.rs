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
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

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

        let execution = async {
            let (status, stdout, stderr) = tokio::join!(
                child.wait(),
                read_bounded(stdout, STDOUT_LIMIT),
                read_bounded(stderr, STDERR_LIMIT),
            );
            Ok::<_, std::io::Error>((status?, stdout?, stderr?))
        };
        let result = Box::pin(tokio::time::timeout(timeout, execution)).await;
        let (status, stdout, stderr) = if let Ok(result) = result {
            result.map_err(|err| ToolError::new(format!("phux CLI I/O failed: {err}")))?
        } else {
            let _ = child.kill().await;
            return Err(ToolError::new(format!(
                "phux CLI exceeded the {}s tool deadline",
                timeout.as_secs_f64()
            )));
        };
        if stdout.truncated {
            return Err(ToolError::new(format!(
                "phux CLI stdout exceeded {STDOUT_LIMIT} bytes"
            )));
        }
        if stderr.truncated {
            return Err(ToolError::new(format!(
                "phux CLI stderr exceeded {STDERR_LIMIT} bytes"
            )));
        }
        let stdout = String::from_utf8_lossy(&stdout.bytes).into_owned();
        let stderr = String::from_utf8_lossy(&stderr.bytes).into_owned();
        if !status.success() && !status.code().is_some_and(|code| allowed.contains(&code)) {
            let message = stderr.trim();
            return Err(ToolError::new(if message.is_empty() {
                format!("phux CLI exited with {status}")
            } else {
                message.to_owned()
            }));
        }
        Ok(CliOutput { stdout })
    }
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

    #[test]
    fn strict_argument_parsers_reject_wrong_shapes_and_bounds() {
        assert!(strict_object(&json!({ "x": 1 }), &["x"], &["x"]).is_ok());
        assert!(strict_object(&json!({ "extra": 1 }), &[], &[]).is_err());
        assert!(bounded_string(&json!({}), "x", true).is_err());
        assert!(bounded_strings(&json!({ "x": [1] }), "x", true).is_err());
        assert!(ratio(&json!({ "ratio": 0.0 })).is_err());
    }
}
