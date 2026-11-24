//! The curl transport behind every GitHub request: one shared request
//! budget, HEAD before GET, api.github.com only, token redaction.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{API_ROOT, Call, Error, Get, Response};

pub(super) struct CurlClient {
    pub(super) token: Option<String>,
    pub(super) budget: Arc<AtomicU32>,
}

impl CurlClient {
    fn charge(&self) -> bool {
        loop {
            let left = self.budget.load(Ordering::Relaxed);
            if left == 0 {
                return false;
            }
            if self
                .budget
                .compare_exchange(left, left - 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }
}

pub(super) fn exchange(
    client: &CurlClient,
    url: &str,
    accept: &str,
    limit: u64,
    head: bool,
) -> Result<Call, Error> {
    if !client.charge() {
        return Ok(Call::Capped);
    }
    client.perform(url, accept, limit, head).map(Call::Done)
}

impl Get for CurlClient {
    fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Response, Error> {
        if !self.charge() {
            return Err(Error::new("request cap reached"));
        }
        self.perform(url, accept, limit, false)
    }
}

impl CurlClient {
    fn perform(&self, url: &str, accept: &str, limit: u64, head: bool) -> Result<Response, Error> {
        if !url.starts_with(API_ROOT) {
            return Err(Error::new("refusing a request outside api.github.com"));
        }
        let dir = crate::validate::scratch()?;
        let header_path = dir.path().join("headers");
        let body_path = dir.path().join("body");
        let config = curl_config(accept, self.token.as_deref());
        let mut command = Command::new("curl");
        command
            .arg("--silent")
            .arg("--show-error")
            .arg("--location")
            .arg("--max-redirs")
            .arg("3")
            .arg("--max-time")
            .arg("30")
            .arg("--proto")
            .arg("=https")
            .arg("--config")
            .arg("-")
            .arg("--dump-header")
            .arg(&header_path);
        if head {
            command.arg("--head").arg("--output").arg("/dev/null");
        } else {
            command
                .arg("--max-filesize")
                .arg(limit.to_string())
                .arg("--output")
                .arg(&body_path);
        }
        let mut child = command
            .arg("--write-out")
            .arg("%{http_code} %{url_effective}")
            .arg(url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error::new(format!("could not run curl (needed for GitHub): {err}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(config.as_bytes())
                .map_err(|err| Error::new(redact(&err.to_string(), self.token.as_deref())))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|err| Error::new(redact(&err.to_string(), self.token.as_deref())))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = redact(
            &String::from_utf8_lossy(&output.stderr),
            self.token.as_deref(),
        );
        if output.status.code() == Some(63) {
            return Ok(Response {
                status: 200,
                headers: format!("content-length: {}\n", limit.saturating_add(1)),
                body: Vec::new(),
            });
        }
        if !output.status.success() {
            return Err(Error::new(format!(
                "curl failed ({status}): {stderr}",
                status = output.status
            )));
        }
        let mut parts = stdout.split_whitespace();
        let status = parts.next().unwrap_or("0").parse::<u16>().unwrap_or(0);
        let final_url = parts.next().unwrap_or("");
        if !final_url.starts_with(API_ROOT) {
            return Err(Error::new("GitHub response URL left api.github.com"));
        }
        let headers = fs::read_to_string(&header_path).unwrap_or_default();
        let len = if head {
            0
        } else {
            fs::metadata(&body_path).map(|meta| meta.len()).unwrap_or(0)
        };
        // The announced size was checked before this GET. A body that still
        // grew past the cap is not stored.
        let body = if head || len > limit {
            Vec::new()
        } else {
            fs::read(&body_path).unwrap_or_default()
        };
        let headers = if !head && len > limit {
            format!("content-length: {len}\n")
        } else {
            last_header_block(&headers).to_owned()
        };
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

fn last_header_block(raw: &str) -> &str {
    let mut last = raw;
    for part in raw.split("\r\n\r\n") {
        if part.starts_with("HTTP/") {
            last = part;
        }
    }
    last
}

pub(crate) fn curl_config(accept: &str, token: Option<&str>) -> String {
    let mut lines = vec![
        "user-agent = \"phux-plugin-index\"".to_owned(),
        format!("header = \"Accept: {accept}\""),
        "header = \"X-GitHub-Api-Version: 2022-11-28\"".to_owned(),
    ];
    if let Some(token) = token {
        lines.push(format!("header = \"Authorization: Bearer {token}\""));
    }
    lines.join("\n")
}

fn redact(text: &str, token: Option<&str>) -> String {
    match token {
        Some(token) if !token.is_empty() => text.replace(token, "***"),
        _ => text.to_owned(),
    }
}
