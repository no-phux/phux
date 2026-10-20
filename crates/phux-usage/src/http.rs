//! HTTPS GET through the system `curl`.
//!
//! The request, bearer token included, goes to curl on stdin as a config
//! file, so it never appears in a process listing; `-q` keeps a user's
//! `.curlrc` from adding `--verbose` and echoing it to stderr.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Per-request ceiling. Plugin hooks are killed after 30 s and the sources
/// are fetched in parallel, so one slow vendor cannot starve the publish.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// A completed HTTP exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: Vec<u8>,
}

/// One HTTP GET. A seam so tests never touch the network.
pub trait Fetch: Sync {
    /// GET `url` with `headers`. `Err` is a transport failure (no status).
    ///
    /// # Errors
    ///
    /// When no HTTP status was received.
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<Reply, String>;
}

/// [`Fetch`] through `/usr/bin/env curl`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Curl;

/// Marker between the body and the status code curl appends.
const STATUS_MARK: &str = "\n@@phux-usage-status@@";

impl Fetch for Curl {
    fn get(&self, url: &str, headers: &[(&str, String)]) -> Result<Reply, String> {
        let mut child = Command::new("curl")
            .args(["-q", "--silent", "--show-error", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("cannot run curl: {err}"))?;
        let config = curl_config(url, headers);
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.as_bytes())
                .map_err(|err| format!("cannot write to curl: {err}"))?;
        }
        let deadline = Instant::now() + REQUEST_TIMEOUT + Duration::from_secs(2);
        let mut stdout = child.stdout.take();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(pipe) = stdout.as_mut() {
                let _ = pipe.read_to_end(&mut out);
            }
            out
        });
        let status = loop {
            match child.try_wait().map_err(|err| err.to_string())? {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("request timed out".to_owned());
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let out = reader.join().unwrap_or_default();
        if !status.success() {
            // curl's own message names the failure (DNS, TLS, timeout) and
            // never the request headers, which only ever travel on stdin.
            let mut err = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut err);
            }
            let line = err.lines().next().unwrap_or("curl failed").trim();
            return Err(line.trim_start_matches("curl: ").to_owned());
        }
        split_status(&out).ok_or_else(|| "curl printed no status".to_owned())
    }
}

/// The body and the status code `write-out` appended after it.
fn split_status(out: &[u8]) -> Option<Reply> {
    let mark = STATUS_MARK.as_bytes();
    let at = out.windows(mark.len()).rposition(|w| w == mark)?;
    let code = std::str::from_utf8(&out[at + mark.len()..]).ok()?.trim();
    Some(Reply {
        status: code.parse().ok()?,
        body: out[..at].to_vec(),
    })
}

/// A curl config with every value quoted under curl's escape rules, so no
/// byte of a header value can start another option.
fn curl_config(url: &str, headers: &[(&str, String)]) -> String {
    let mut config = format!(
        "url = {}\nmax-time = {}\nwrite-out = {}\n",
        quote(url),
        REQUEST_TIMEOUT.as_secs(),
        quote(&format!("{STATUS_MARK}%{{http_code}}")),
    );
    for (name, value) in headers {
        config.push_str("header = ");
        config.push_str(&quote(&format!("{name}: {value}")));
        config.push('\n');
    }
    config
}

fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_quotes_every_value() {
        let config = curl_config(
            "https://x/y?a=1",
            &[("Authorization", "Bearer a\"b\nc".to_owned())],
        );
        assert!(config.contains("url = \"https://x/y?a=1\"\n"));
        assert!(config.contains("header = \"Authorization: Bearer a\\\"b\\nc\"\n"));
        assert_eq!(
            config.lines().count(),
            4,
            "a newline in a value never adds an option"
        );
    }

    #[test]
    fn status_is_split_from_the_body() {
        let reply = split_status(b"{\"a\":1}\n@@phux-usage-status@@429").unwrap();
        assert_eq!(reply.status, 429);
        assert_eq!(reply.body, b"{\"a\":1}");
        assert!(split_status(b"no marker").is_none());
    }
}
