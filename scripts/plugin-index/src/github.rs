//! Public GitHub REST reads: topic search and file contents.
//!
//! Unauthenticated calls work. A token, when the caller has one, is sent
//! only as an Authorization header so the rate limit is higher. Offline
//! builds never construct a client.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::{Error, Source};

const SEARCH_URL: &str =
    "https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100";
const API_ROOT: &str = "https://api.github.com/";
const FILE_LIMIT: u64 = 1024 * 1024 + 1;
const SEARCH_LIMIT: u64 = 2 * 1024 * 1024;
const MAX_PAGES: usize = 10;
const MAX_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub(crate) struct RepoMeta {
    pub full_name: String,
    pub archived: bool,
    pub fork: bool,
    pub private: bool,
    pub default_branch: String,
    pub stars: u64,
    pub pushed_at: String,
    pub license: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogMeta {
    pub stars: Option<u64>,
    pub pushed_at: Option<String>,
    pub license: Option<String>,
}

impl From<&RepoMeta> for CatalogMeta {
    fn from(repo: &RepoMeta) -> Self {
        Self {
            stars: Some(repo.stars),
            pushed_at: Some(repo.pushed_at.clone()),
            license: repo.license.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchBody {
    incomplete_results: bool,
    items: Vec<RawRepo>,
}

#[derive(Debug, Deserialize)]
struct RawRepo {
    full_name: String,
    archived: bool,
    fork: bool,
    #[serde(default)]
    private: bool,
    default_branch: String,
    stargazers_count: u64,
    pushed_at: String,
    #[serde(default)]
    license: Option<RawLicense>,
}

#[derive(Debug, Deserialize)]
struct RawLicense {
    #[serde(default)]
    spdx_id: Option<String>,
}

struct Response {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

trait Get {
    fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Response, Error>;
}

pub(crate) fn search(source: &Source) -> Result<Vec<RepoMeta>, Error> {
    match source {
        Source::Offline { fixtures } => offline_search(fixtures),
        Source::Github { token } => {
            let client = CurlClient {
                token: token.clone(),
            };
            collect_search(&client, &mut |delay| std::thread::sleep(delay))
        }
    }
}

pub(crate) fn read_text(
    source: &Source,
    repo: &RepoMeta,
    path: &str,
) -> Result<Option<String>, Error> {
    match source {
        Source::Offline { fixtures } => offline_file(fixtures, &repo.full_name, path),
        Source::Github { token } => {
            let client = CurlClient {
                token: token.clone(),
            };
            let url = contents_url(&repo.full_name, &repo.default_branch, path)?;
            http_text(&client, &url, "application/vnd.github.raw", FILE_LIMIT)
        }
    }
}

pub(crate) fn fetch_repo(source: &Source, full_name: &str) -> Result<Option<RepoMeta>, Error> {
    if !valid_repo_name(full_name) {
        return Err(Error::new(format!(
            "example repository {full_name} is not owner/repo"
        )));
    }
    match source {
        Source::Offline { fixtures } => {
            let path = repo_dir(fixtures, full_name)?.join("repo.json");
            if !path.is_file() {
                return Ok(None);
            }
            let text = fs::read_to_string(&path)
                .map_err(|err| Error::new(format!("reading {}: {err}", path.display())))?;
            let raw: RawRepo = serde_json::from_str(&text)
                .map_err(|err| Error::new(format!("{}: {err}", path.display())))?;
            let repo = normalize_repo(raw)?;
            if repo.full_name != full_name {
                return Err(Error::new(format!(
                    "{} names {}, expected {full_name}",
                    path.display(),
                    repo.full_name
                )));
            }
            Ok(Some(repo))
        }
        Source::Github { token } => {
            let client = CurlClient {
                token: token.clone(),
            };
            let url = format!("{API_ROOT}repos/{full_name}");
            match http_text(&client, &url, "application/vnd.github+json", SEARCH_LIMIT)? {
                None => Ok(None),
                Some(text) => {
                    let raw: RawRepo = serde_json::from_str(&text).map_err(|err| {
                        Error::new(format!("repository {full_name} metadata: {err}"))
                    })?;
                    Ok(Some(normalize_repo(raw)?))
                }
            }
        }
    }
}

fn offline_search(fixtures: &Path) -> Result<Vec<RepoMeta>, Error> {
    let path = fixtures.join("search.json");
    let text = fs::read_to_string(&path)
        .map_err(|err| Error::new(format!("reading {}: {err}", path.display())))?;
    parse_search_page(&text)
}

fn parse_search_page(text: &str) -> Result<Vec<RepoMeta>, Error> {
    let body: SearchBody = serde_json::from_str(text)
        .map_err(|err| Error::new(format!("GitHub search response: {err}")))?;
    if body.incomplete_results {
        return Err(Error::new(
            "GitHub search returned incomplete_results; refusing a partial index",
        ));
    }
    body.items.into_iter().map(normalize_repo).collect()
}

fn normalize_repo(raw: RawRepo) -> Result<RepoMeta, Error> {
    if raw.full_name.contains(['\n', '\r']) {
        return Err(Error::new("search result name contains a newline"));
    }
    if !valid_repo_name(&raw.full_name) {
        return Err(Error::new(format!(
            "search result {} is not a usable owner/repo",
            raw.full_name
        )));
    }
    Ok(RepoMeta {
        full_name: raw.full_name,
        archived: raw.archived,
        fork: raw.fork,
        private: raw.private,
        default_branch: raw.default_branch,
        stars: raw.stargazers_count,
        pushed_at: raw.pushed_at,
        license: license_id(raw.license),
    })
}

fn license_id(license: Option<RawLicense>) -> Option<String> {
    let id = license.and_then(|license| license.spdx_id)?;
    if id.is_empty() || id == "NOASSERTION" || id == "NONE" {
        None
    } else {
        Some(id)
    }
}

pub(crate) fn valid_repo_name(name: &str) -> bool {
    let Some((owner, repo)) = name.split_once('/') else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    ok(owner) && ok(repo)
}

fn offline_file(fixtures: &Path, repo: &str, path: &str) -> Result<Option<String>, Error> {
    let base = repo_dir(fixtures, repo)?;
    if !base.is_dir() {
        return Ok(None);
    }
    crate::validate::safe_relative(path).map_err(Error::new)?;
    let full = base.join(path);
    if !full.is_file() {
        return Ok(None);
    }
    let canon_base = base
        .canonicalize()
        .map_err(|err| Error::new(format!("fixture {}: {err}", base.display())))?;
    let canon = full
        .canonicalize()
        .map_err(|err| Error::new(format!("fixture {}: {err}", full.display())))?;
    if !canon.starts_with(&canon_base) {
        return Err(Error::new("fixture path escapes the repository directory"));
    }
    let len = fs::metadata(&canon)
        .map_err(|err| Error::new(err.to_string()))?
        .len();
    if len > FILE_LIMIT {
        return Err(Error::new(format!(
            "{} exceeds the manifest size limit",
            full.display()
        )));
    }
    fs::read_to_string(&canon)
        .map(Some)
        .map_err(|err| Error::new(format!("reading {}: {err}", full.display())))
}

fn repo_dir(fixtures: &Path, repo: &str) -> Result<PathBuf, Error> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| Error::new(format!("{repo} is not owner/repo")))?;
    Ok(fixtures.join("repos").join(owner).join(name))
}

fn contents_url(repo: &str, branch: &str, path: &str) -> Result<String, Error> {
    if branch.is_empty() || branch.contains(['/', '?', '#']) {
        return Err(Error::new(format!("{repo} has no usable default branch")));
    }
    let encoded = path
        .split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!(
        "{API_ROOT}repos/{repo}/contents/{encoded}?ref={}",
        percent_encode(branch)
    ))
}

fn collect_search(
    client: &dyn Get,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Vec<RepoMeta>, Error> {
    let mut url = SEARCH_URL.to_owned();
    let mut repos = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for page in 0..MAX_PAGES {
        let response = get_retrying(
            client,
            &url,
            "application/vnd.github+json",
            SEARCH_LIMIT,
            sleep,
        )?;
        if response.status != 200 {
            return Err(Error::new(format!(
                "GitHub search returned HTTP {}",
                response.status
            )));
        }
        if !response_url_ok(&url) {
            return Err(Error::new("search URL left api.github.com"));
        }
        let text = std::str::from_utf8(&response.body)
            .map_err(|_| Error::new("GitHub search response was not UTF-8"))?;
        let batch = parse_search_page(text)?;
        if batch.is_empty()
            && header(&response.headers, "link").is_some_and(|link| next_link(link).is_some())
        {
            return Err(Error::new(
                "GitHub search returned an empty page with a next link",
            ));
        }
        for repo in batch {
            if seen.insert(repo.full_name.clone()) {
                repos.push(repo);
            }
        }
        let Some(next) = header(&response.headers, "link").and_then(next_link) else {
            return Ok(repos);
        };
        if !next.starts_with(API_ROOT) {
            return Err(Error::new("GitHub search next link left api.github.com"));
        }
        url = next.to_owned();
        if page + 1 == MAX_PAGES {
            return Err(Error::new(
                "GitHub search exceeded 1000 repositories; refusing a partial index",
            ));
        }
    }
    Ok(repos)
}

fn http_text(
    client: &dyn Get,
    url: &str,
    accept: &str,
    limit: u64,
) -> Result<Option<String>, Error> {
    let response = get_retrying(client, url, accept, limit, &mut |delay| {
        std::thread::sleep(delay)
    })?;
    if response.status == 404 {
        return Ok(None);
    }
    if response.status != 200 {
        return Err(Error::new(format!(
            "GitHub returned HTTP {} for {url}",
            response.status
        )));
    }
    let text = String::from_utf8(response.body)
        .map_err(|_| Error::new(format!("GitHub file was not UTF-8: {url}")))?;
    Ok(Some(text))
}

fn response_url_ok(url: &str) -> bool {
    url.starts_with(API_ROOT)
}

fn get_retrying(
    client: &dyn Get,
    url: &str,
    accept: &str,
    limit: u64,
    sleep: &mut dyn FnMut(Duration),
) -> Result<Response, Error> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let first = client.get(url, accept, limit)?;
    if let Some(delay) = retry_wait(first.status, &first.headers, &first.body, now) {
        if delay > MAX_WAIT {
            return Err(Error::new(
                "GitHub rate limit needs longer than 20s; set GITHUB_TOKEN to raise it",
            ));
        }
        sleep(delay);
        let second = client.get(url, accept, limit)?;
        if second.status == 403 || second.status == 429 {
            return Err(Error::new(format!(
                "GitHub rate limit persisted (HTTP {})",
                second.status
            )));
        }
        return Ok(second);
    }
    Ok(first)
}

fn retry_wait(status: u16, headers: &str, body: &[u8], now: u64) -> Option<Duration> {
    if status != 403 && status != 429 {
        return None;
    }
    if let Some(after) = header(headers, "retry-after").and_then(|text| text.parse::<u64>().ok()) {
        return Some(Duration::from_secs(after));
    }
    if header(headers, "x-ratelimit-remaining") == Some("0") {
        if let Some(reset) =
            header(headers, "x-ratelimit-reset").and_then(|text| text.parse::<u64>().ok())
        {
            return Some(Duration::from_secs(reset.saturating_sub(now)));
        }
    }
    let text = String::from_utf8_lossy(body);
    if text.to_ascii_lowercase().contains("rate limit") {
        return Some(Duration::from_secs(2));
    }
    None
}

fn header<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let mut found = None;
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case(name) {
            found = Some(value.trim());
        }
    }
    found
}

fn next_link(value: &str) -> Option<&str> {
    for part in value.split(',') {
        if part.contains("rel=\"next\"") {
            let start = part.find('<')? + 1;
            let end = part.find('>')?;
            if start < end {
                return Some(&part[start..end]);
            }
        }
    }
    None
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

struct CurlClient {
    token: Option<String>,
}

impl Get for CurlClient {
    fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Response, Error> {
        if !url.starts_with(API_ROOT) {
            return Err(Error::new("refusing a request outside api.github.com"));
        }
        let dir = crate::validate::scratch()?;
        let header_path = dir.path().join("headers");
        let body_path = dir.path().join("body");
        let config = curl_config(accept, self.token.as_deref());
        let mut child = Command::new("curl")
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
            .arg(&header_path)
            .arg("--output")
            .arg(&body_path)
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
        let len = fs::metadata(&body_path).map(|meta| meta.len()).unwrap_or(0);
        if len > limit {
            return Err(Error::new(format!(
                "GitHub response exceeded {limit} bytes"
            )));
        }
        let body = fs::read(&body_path).unwrap_or_default();
        Ok(Response {
            status,
            headers: last_header_block(&headers).to_owned(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_includes_the_topic_and_forks() {
        assert!(SEARCH_URL.contains("topic%3Aphux-plugin"));
        assert!(SEARCH_URL.contains("fork%3Atrue"));
        assert!(SEARCH_URL.starts_with(API_ROOT));
    }

    #[test]
    fn curl_config_sends_a_token_only_when_one_was_provided() {
        let plain = curl_config("application/vnd.github+json", None);
        assert!(!plain.contains("Authorization"));
        let authed = curl_config("application/vnd.github.raw", Some("ghp_example"));
        assert!(authed.contains("Authorization: Bearer ghp_example"));
        assert!(authed.contains("phux-plugin-index"));
    }

    #[test]
    fn license_ids_drop_github_placeholders() {
        assert_eq!(
            license_id(Some(RawLicense {
                spdx_id: Some("NOASSERTION".to_owned())
            })),
            None
        );
        assert_eq!(
            license_id(Some(RawLicense {
                spdx_id: Some("MIT".to_owned())
            }))
            .as_deref(),
            Some("MIT")
        );
    }

    #[test]
    fn next_link_is_the_rel_next_url() {
        let block = "Link: <https://api.github.com/search/repositories?q=a&page=2>; rel=\"next\", <https://api.github.com/search/repositories?q=a&page=3>; rel=\"last\"";
        assert_eq!(
            next_link(header(block, "link").unwrap()),
            Some("https://api.github.com/search/repositories?q=a&page=2")
        );
    }

    struct Scripted {
        calls: std::sync::Mutex<Vec<String>>,
        pages: Vec<(&'static str, u16, &'static str, &'static str)>,
    }

    impl Get for Scripted {
        fn get(&self, url: &str, _accept: &str, _limit: u64) -> Result<Response, Error> {
            self.calls.lock().expect("lock").push(url.to_owned());
            let page = self
                .pages
                .iter()
                .find(|(want, _, _, _)| *want == url)
                .ok_or_else(|| Error::new(format!("unexpected url {url}")))?;
            Ok(Response {
                status: page.1,
                headers: page.2.to_owned(),
                body: page.3.as_bytes().to_vec(),
            })
        }
    }

    #[test]
    fn search_follows_the_next_link_and_stops() {
        let second = "https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100&page=2";
        let client = Scripted {
            calls: std::sync::Mutex::new(Vec::new()),
            pages: vec![
                (
                    SEARCH_URL,
                    200,
                    "Link: <https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100&page=2>; rel=\"next\"\n",
                    r#"{"incomplete_results":false,"items":[{"full_name":"acme/one","archived":false,"fork":false,"default_branch":"main","stargazers_count":1,"pushed_at":"2026-01-01T00:00:00Z","license":null}]}"#,
                ),
                (
                    second,
                    200,
                    "\n",
                    r#"{"incomplete_results":false,"items":[{"full_name":"acme/two","archived":false,"fork":true,"default_branch":"trunk","stargazers_count":2,"pushed_at":"2026-01-02T00:00:00Z","license":{"spdx_id":"Apache-2.0"}}]}"#,
                ),
            ],
        };
        let mut slept = 0;
        let repos = collect_search(&client, &mut |_| slept += 1).expect("search");
        assert_eq!(slept, 0);
        assert_eq!(repos.len(), 2);
        assert!(repos[1].fork);
        assert_eq!(repos[1].license.as_deref(), Some("Apache-2.0"));
        assert_eq!(client.calls.lock().expect("lock").len(), 2);
    }

    #[test]
    fn incomplete_search_is_an_error() {
        let err = parse_search_page(r#"{"incomplete_results":true,"items":[]}"#).unwrap_err();
        assert!(err.to_string().contains("incomplete_results"), "{err}");
    }

    #[test]
    fn a_long_rate_limit_wait_is_refused() {
        let wait = retry_wait(403, "retry-after: 60\n", b"", 0);
        assert!(wait.is_some_and(|delay| delay > MAX_WAIT));
    }
}
