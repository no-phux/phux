//! Public GitHub REST reads: topic search and file contents.
//!
//! Unauthenticated calls work. A token, when the caller has one, is sent
//! only as an Authorization header so the rate limit is higher. Offline
//! builds never construct a client.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::{Error, Source};

mod curl;

use curl::{CurlClient, exchange};

const SEARCH_URL: &str =
    "https://api.github.com/search/repositories?q=topic%3Aphux-plugin+fork%3Atrue&per_page=100";
const API_ROOT: &str = "https://api.github.com/";
/// A manifest larger than this is skipped. Checked from `Content-Length` or
/// the file size before the body is stored.
const FILE_LIMIT: u64 = 1024 * 1024;

/// One repository file. [`Fetched::Skip`] is that repository only.
#[derive(Debug)]
pub(crate) enum Fetched {
    Text(String),
    Missing,
    Skip(String),
}
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
        Source::Github { token, budget } => {
            let client = CurlClient {
                token: token.clone(),
                budget: Arc::clone(budget),
            };
            collect_search(&client, &mut |delay| std::thread::sleep(delay))
        }
    }
}

pub(crate) fn read_text(source: &Source, repo: &RepoMeta, path: &str) -> Result<Fetched, Error> {
    match source {
        Source::Offline { fixtures } => offline_file(fixtures, &repo.full_name, path),
        Source::Github { token, budget } => {
            let client = CurlClient {
                token: token.clone(),
                budget: Arc::clone(budget),
            };
            let url = contents_url(&repo.full_name, &repo.default_branch, path)?;
            read_online(&client, &url, "application/vnd.github.raw", FILE_LIMIT)
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
        Source::Github { token, budget } => {
            let client = CurlClient {
                token: token.clone(),
                budget: Arc::clone(budget),
            };
            let url = format!("{API_ROOT}repos/{full_name}");
            repo_from_fetched(read_online(
                &client,
                &url,
                "application/vnd.github+json",
                SEARCH_LIMIT,
            )?)
        }
    }
}

/// A missing or unparseable repository document is `None` (the example is
/// dropped). A skip is an error: unlike a manifest, repository metadata has
/// no "this one is just malformed" case, so a 403 or an oversized body means
/// the build cannot vouch for the index.
fn repo_from_fetched(fetched: Fetched) -> Result<Option<RepoMeta>, Error> {
    match fetched {
        Fetched::Text(text) => match serde_json::from_str::<RawRepo>(&text) {
            Ok(raw) => Ok(normalize_repo(raw).ok()),
            Err(_) => Ok(None),
        },
        Fetched::Missing => Ok(None),
        Fetched::Skip(reason) => Err(Error::new(format!(
            "could not read repository metadata: {reason}"
        ))),
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

fn offline_file(fixtures: &Path, repo: &str, path: &str) -> Result<Fetched, Error> {
    let base = repo_dir(fixtures, repo)?;
    if !base.is_dir() {
        return Ok(Fetched::Missing);
    }
    if crate::validate::safe_relative(path).is_err() {
        return Ok(Fetched::Skip("invalid plugin directory".to_owned()));
    }
    let full = base.join(path);
    if full.is_dir() {
        return Ok(Fetched::Skip("manifest path is a directory".to_owned()));
    }
    if !full.is_file() {
        return Ok(Fetched::Missing);
    }
    let canon_base = match base.canonicalize() {
        Ok(path) => path,
        Err(err) => return Ok(Fetched::Skip(format!("could not read manifest: {err}"))),
    };
    let canon = match full.canonicalize() {
        Ok(path) => path,
        Err(err) => return Ok(Fetched::Skip(format!("could not read manifest: {err}"))),
    };
    if !canon.starts_with(&canon_base) {
        return Ok(Fetched::Skip(
            "fixture path escapes the repository directory".to_owned(),
        ));
    }
    let len = match fs::metadata(&canon) {
        Ok(meta) => meta.len(),
        Err(err) => return Ok(Fetched::Skip(format!("could not read manifest: {err}"))),
    };
    if len > FILE_LIMIT {
        return Ok(Fetched::Skip("manifest exceeds 1 MiB".to_owned()));
    }
    let bytes = match fs::read(&canon) {
        Ok(bytes) => bytes,
        Err(err) => return Ok(Fetched::Skip(format!("could not read manifest: {err}"))),
    };
    match String::from_utf8(bytes) {
        Ok(text) => Ok(Fetched::Text(text)),
        Err(_) => Ok(Fetched::Skip("manifest is not UTF-8".to_owned())),
    }
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

fn read_online(client: &CurlClient, url: &str, accept: &str, limit: u64) -> Result<Fetched, Error> {
    read_online_with(|head| exchange(client, url, accept, limit, head), limit)
}

/// `fetch(true)` is the header request. `fetch(false)` is the body.
/// Transport errors and an exhausted request cap fail the build. A rate-limit
/// response fails the build, including a HEAD 403 whose body only shows up on GET.
fn read_online_with(
    mut fetch: impl FnMut(bool) -> Result<Call, Error>,
    limit: u64,
) -> Result<Fetched, Error> {
    let head = match fetch(true)? {
        Call::Done(head) => head,
        Call::Capped => return Err(Error::new("request cap reached")),
    };
    if rate_limited(head.status, &head.headers, &head.body) {
        return Err(Error::new(
            "GitHub rate limit needs longer than 20s; set GITHUB_TOKEN to raise it",
        ));
    }
    if let Some(len) = content_length(&head.headers) {
        if len > limit {
            return Ok(Fetched::Skip("manifest exceeds 1 MiB".to_owned()));
        }
    }
    // A HEAD 403 has no body. Secondary rate limits often say so only in the
    // GET body, with no retry-after on the header response.
    if head.status == 403 {
        let got = match fetch(false)? {
            Call::Done(got) => got,
            Call::Capped => return Err(Error::new("request cap reached")),
        };
        if rate_limited(got.status, &got.headers, &got.body) {
            return Err(Error::new(
                "GitHub rate limit needs longer than 20s; set GITHUB_TOKEN to raise it",
            ));
        }
        return classify_body(got.status, &got.headers, got.body, limit);
    }
    if head.status == 404 {
        return Ok(Fetched::Missing);
    }
    if head.status != 200 {
        return Ok(Fetched::Skip(format!("HTTP {}", head.status)));
    }
    let got = match fetch(false)? {
        Call::Done(got) => got,
        Call::Capped => return Err(Error::new("request cap reached")),
    };
    if let Some(len) = content_length(&got.headers) {
        if len > limit {
            return Ok(Fetched::Skip("manifest exceeds 1 MiB".to_owned()));
        }
    }
    if rate_limited(got.status, &got.headers, &got.body) {
        return Err(Error::new(
            "GitHub rate limit needs longer than 20s; set GITHUB_TOKEN to raise it",
        ));
    }
    classify_body(got.status, &got.headers, got.body, limit)
}

pub(crate) fn classify_body(
    status: u16,
    headers: &str,
    body: Vec<u8>,
    limit: u64,
) -> Result<Fetched, Error> {
    // Rate limits are decided in `read_online_with` before this, so a caller
    // that skips those checks cannot still fail closed here.
    if status == 404 {
        return Ok(Fetched::Missing);
    }
    if status == 403 {
        return Ok(Fetched::Skip("HTTP 403".to_owned()));
    }
    if status != 200 {
        return Ok(Fetched::Skip(format!("HTTP {status}")));
    }
    if body.len() as u64 > limit {
        return Ok(Fetched::Skip("manifest exceeds 1 MiB".to_owned()));
    }
    if header(headers, "content-type").is_some_and(|kind| kind.contains("application/json"))
        && body.first().is_none_or(|byte| *byte == b'[')
    {
        return Ok(Fetched::Skip("manifest path is a directory".to_owned()));
    }
    match String::from_utf8(body) {
        Ok(text) => Ok(Fetched::Text(text)),
        Err(_) => Ok(Fetched::Skip("manifest is not UTF-8".to_owned())),
    }
}

fn rate_limited(status: u16, headers: &str, body: &[u8]) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    status == 429 || (status == 403 && retry_wait(status, headers, body, now).is_some())
}

fn content_length(headers: &str) -> Option<u64> {
    header(headers, "content-length").and_then(|text| text.parse().ok())
}

enum Call {
    Done(Response),
    Capped,
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

#[cfg(test)]
mod tests;
