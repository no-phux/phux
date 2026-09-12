//! The release artifact contract `phux update` consumes, plus the one
//! injectable boundary the whole command is tested through.
//!
//! The naming here is not invented: it mirrors `.github/workflows/release.yml`
//! exactly (`phux-<tag>-<target>.tar.gz` staged from a `phux-<tag>-<target>/`
//! directory, with a `<archive>.sha256` sidecar written as
//! `"<64 hex>  <archive>"`). `docs/RELEASING.md` writes the same layout down
//! for humans. If the workflow's naming ever moves, this module and that doc
//! move with it.
//!
//! [`ReleaseSource`] is the seam. Everything that touches the network lives
//! behind it, so the update logic — version comparison, checksum
//! verification, staging, atomic replacement, rollback — is exercised in unit
//! tests against a local fake and never performs a real download in CI.

use std::io::Read as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use super::UpdateError;

/// The repository releases are published from.
pub(crate) const REPO: &str = "no-phux/phux";

/// Match the standalone installers. Index discovery is bounded, and an
/// explicit --version bypasses it if the stream is older than this window.
const MAX_RELEASE_PAGES: usize = 10;
const MAX_RELEASE_PAGE_BYTES: u64 = 1_048_576;

/// A parsed `MAJOR.MINOR.PATCH`.
///
/// Release tags are strictly `vX.Y.Z` (release-please owns them; see
/// `docs/RELEASING.md`), so a three-field ordered tuple is the whole of the
/// comparison. Pre-release and build metadata are deliberately unsupported:
/// a tag carrying either is not something this lane publishes, and silently
/// dropping the suffix would make two different releases compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Version {
    /// Parse `X.Y.Z` or `vX.Y.Z`. Returns `None` for anything else,
    /// including a pre-release or build-metadata suffix.
    ///
    /// Deliberately strict about surrounding whitespace: this feeds
    /// [`validate_tag`], which decides whether a string is safe to
    /// interpolate into a download URL, and `"v1.2.3 "` is not the same
    /// resource as `"v1.2.3"`. Callers that read a tag out of a program's
    /// output trim it themselves, on purpose, before it gets here.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let body = text.strip_prefix('v').unwrap_or(text);
        let mut fields = body.split('.');
        let major = version_component(fields.next()?)?;
        let minor = version_component(fields.next()?)?;
        let patch = version_component(fields.next()?)?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// The version this binary was built as.
    pub(crate) fn current() -> Option<Self> {
        Self::parse(env!("CARGO_PKG_VERSION"))
    }
}

/// Rust integer parsing accepts a leading + and zero padding; release tags do
/// not. Keep URL validation aligned with the standalone installers.
fn version_component(text: &str) -> Option<u64> {
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Validate a release tag before it is ever interpolated into a URL.
///
/// The tag arrives either from `--version` (a human) or from a redirect's
/// final path segment (a remote server). Neither is trusted to be free of
/// `../`, a query string, or a scheme; requiring the exact `vX.Y.Z` shape
/// means a hostile redirect cannot steer the download anywhere except at a
/// tag that does not exist.
pub(crate) fn validate_tag(tag: &str) -> Result<Version, UpdateError> {
    let version = Version::parse(tag)
        .filter(|_| tag.starts_with('v'))
        .ok_or_else(|| {
            UpdateError::InvalidTag(format!(
                "`{tag}` is not a release tag; releases are tagged vMAJOR.MINOR.PATCH"
            ))
        })?;
    Ok(version)
}

/// The Rust target triple this build's release artifact is published under,
/// or an error naming the platform that has none.
///
/// The published set is the `release.yml` matrix: macOS arm64, Linux `x86_64`,
/// Linux arm64. macOS `x86_64` is deliberately absent (see `docs/INSTALL.md`),
/// so it gets its own message rather than a generic refusal.
pub(crate) fn host_target() -> Result<&'static str, UpdateError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Err(UpdateError::UnsupportedPlatform(
            "macOS x86_64 has no published release artifact; build from source".to_owned(),
        )),
        (os, arch) => Err(UpdateError::UnsupportedPlatform(format!(
            "{os}/{arch} has no published release artifact; build from source"
        ))),
    }
}

/// One release's artifact URLs, derived from the tag and the host target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Artifact {
    /// `phux-<tag>-<target>.tar.gz`.
    pub(crate) archive: String,
    /// The directory every member of the archive sits under:
    /// `phux-<tag>-<target>`.
    pub(crate) stage: String,
    /// Where the archive is downloaded from.
    pub(crate) archive_url: String,
    /// Where the `.sha256` sidecar is downloaded from.
    pub(crate) checksum_url: String,
}

impl Artifact {
    /// Build the artifact naming for `tag` on `target`, matching
    /// `release.yml`'s packaging step byte for byte.
    pub(crate) fn new(tag: &str, target: &str) -> Self {
        let stage = format!("phux-{tag}-{target}");
        let archive = format!("{stage}.tar.gz");
        let archive_url = format!("https://github.com/{REPO}/releases/download/{tag}/{archive}");
        let checksum_url = format!("{archive_url}.sha256");
        Self {
            archive,
            stage,
            archive_url,
            checksum_url,
        }
    }
}

/// The one boundary that talks to the network.
///
/// Two operations, both deliberately dumb: name the current release, and put
/// the bytes at a URL into a file. No decisions are delegated across this
/// seam — verification, staging, and replacement all happen on the near side,
/// so a fake in a test exercises exactly the code a real update runs.
pub(crate) trait ReleaseSource: std::fmt::Debug {
    /// The tag of the current stable release (`vX.Y.Z`).
    fn latest_tag(&self) -> Result<String, UpdateError>;

    /// Download `url` into `dest`, replacing whatever is there.
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// The real [`ReleaseSource`]: `curl`, falling back to `wget`.
///
/// phux does not link an HTTP client. The documented install path already
/// requires one of these two tools (`scripts/install.sh` refuses without
/// them, `docs/INSTALL.md` leads with the curl one-liner), and delegating
/// transport to them keeps TLS verification, proxy configuration, and
/// redirect handling in a battle-tested implementation instead of a
/// hand-rolled one inside a terminal multiplexer. The trust anchor is the
/// checksum this crate verifies afterwards, not the fetcher.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkReleaseSource;

/// Which downloader is available on this host.
#[derive(Debug, Clone, Copy)]
enum Downloader {
    Curl,
    Wget,
}

impl Downloader {
    fn detect() -> Result<Self, UpdateError> {
        for (name, downloader) in [("curl", Self::Curl), ("wget", Self::Wget)] {
            if Command::new(name)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
            {
                return Ok(downloader);
            }
        }
        Err(UpdateError::Fetch(
            "neither `curl` nor `wget` is on PATH; one of them is required to \
             download a release"
                .to_owned(),
        ))
    }
}

impl ReleaseSource for NetworkReleaseSource {
    fn latest_tag(&self) -> Result<String, UpdateError> {
        let downloader = Downloader::detect()?;
        resolve_core_tag(|page| fetch_index_page(downloader, page))
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        let downloader = Downloader::detect()?;
        let dest_arg = dest.to_string_lossy().into_owned();
        match downloader {
            Downloader::Curl => run("curl", &["-fsSL", url, "-o", &dest_arg]).map(|_| ()),
            Downloader::Wget => run("wget", &["-q", "-O", &dest_arg, url]).map(|_| ()),
        }
    }
}

fn fetch_index_page(downloader: Downloader, page: usize) -> Result<String, UpdateError> {
    let url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30&page={page}");
    match downloader {
        Downloader::Curl => run_index(
            "curl",
            &["-fsSL", "--connect-timeout", "10", "--max-time", "30", &url],
        ),
        Downloader::Wget => run_index(
            "wget",
            &["-q", "--timeout=30", "--tries=1", "-O", "-", &url],
        ),
    }
}

/// Unlike artifact downloads (which write files), index stdout is untrusted
/// data held in memory. Stop reading and reap the fetcher after at most 1 MiB.
fn run_index(program: &str, args: &[&str]) -> Result<String, UpdateError> {
    let child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        // Preserve the CLI JSON error contract instead of mixing curl/wget
        // diagnostics into stderr. index_error supplies an actionable remedy.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| index_error(format!("could not run {program}: {err}")))?;
    read_index(child)
}

fn read_index(mut child: Child) -> Result<String, UpdateError> {
    let mut body = Vec::new();
    let read = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("missing index stdout"))
        .and_then(|stdout| {
            stdout
                .take(MAX_RELEASE_PAGE_BYTES + 1)
                .read_to_end(&mut body)
        });
    if read.is_err() || body.len() as u64 > MAX_RELEASE_PAGE_BYTES {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|err| index_error(format!("could not reap index fetcher: {err}")))?;
    read.map_err(|err| index_error(format!("could not read release index: {err}")))?;
    check_page_size(body.len())?;
    if !status.success() {
        return Err(index_error(format!(
            "release index fetcher exited with {status}"
        )));
    }
    String::from_utf8(body)
        .map_err(|err| index_error(format!("invalid release list encoding: {err}")))
}

fn index_error(message: impl std::fmt::Display) -> UpdateError {
    UpdateError::Fetch(format!(
        "{message}; check GitHub access/rate limits or pass --version with a known release tag"
    ))
}

fn check_page_size(size: usize) -> Result<(), UpdateError> {
    if size as u64 > MAX_RELEASE_PAGE_BYTES {
        return Err(index_error(format!(
            "release page exceeds {MAX_RELEASE_PAGE_BYTES} bytes"
        )));
    }
    Ok(())
}

/// Injectable page boundary: tests exercise the same loop as the real fetcher.
fn resolve_core_tag(
    mut fetch: impl FnMut(usize) -> Result<String, UpdateError>,
) -> Result<String, UpdateError> {
    for page in 1..=MAX_RELEASE_PAGES {
        let body = fetch(page)?;
        let releases = release_list(&body)?;
        if let Some(tag) = releases.iter().find_map(ReleaseRecord::stable_core_tag) {
            return Ok(tag.to_owned());
        }
        if releases.is_empty() {
            return Err(index_error("no stable core release found"));
        }
    }
    Err(index_error(format!(
        "no stable core release found within {MAX_RELEASE_PAGES} pages"
    )))
}

/// Run `program` with `args`, returning stdout as a string.
fn run(program: &str, args: &[&str]) -> Result<String, UpdateError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| UpdateError::Fetch(format!("could not run `{program}`: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(UpdateError::Fetch(format!(
            "`{program}` exited with {}{suffix}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Required typed metadata rejects missing/duplicate/mistyped fields. Tags
/// alone cannot distinguish stable releases from drafts and prereleases.
#[derive(Debug, serde::Deserialize)]
struct ReleaseRecord {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

impl ReleaseRecord {
    fn stable_core_tag(&self) -> Option<&str> {
        if self.draft || self.prerelease {
            return None;
        }
        validate_tag(&self.tag_name)
            .ok()
            .map(|_| self.tag_name.as_str())
    }
}

fn release_list(body: &str) -> Result<Vec<ReleaseRecord>, UpdateError> {
    check_page_size(body.len())?;
    serde_json::from_str(body).map_err(|err| index_error(format!("invalid release list: {err}")))
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, MAX_RELEASE_PAGE_BYTES, MAX_RELEASE_PAGES, ReleaseRecord, Version, release_list,
        resolve_core_tag, run_index, validate_tag,
    };

    fn latest_core_tag_from_list(body: &str) -> Option<String> {
        release_list(body)
            .ok()?
            .iter()
            .find_map(ReleaseRecord::stable_core_tag)
            .map(str::to_owned)
    }

    #[test]
    fn versions_parse_with_and_without_the_v_prefix_and_order_correctly() {
        assert_eq!(Version::parse("0.12.1"), Version::parse("v0.12.1"));
        let old = Version::parse("v0.12.1").unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
        });
        let new = Version::parse("v0.13.0").unwrap_or(Version {
            major: 0,
            minor: 0,
            patch: 0,
        });
        assert!(new > old);
        assert_eq!(new.to_string(), "0.13.0");
    }

    #[test]
    fn versions_refuse_shapes_the_release_lane_never_publishes() {
        for text in [
            "",
            "v1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3-rc.1",
            "latest",
            "v+1.2.3",
            "v01.2.3",
            "v1.2.3\nevil",
        ] {
            assert!(Version::parse(text).is_none(), "{text} should not parse");
        }
    }

    #[test]
    fn this_build_has_a_parseable_version() {
        assert!(
            Version::current().is_some(),
            "the workspace version must be a plain MAJOR.MINOR.PATCH"
        );
    }

    /// A tag is interpolated straight into a download URL, so anything that
    /// is not exactly `vX.Y.Z` is refused before it gets there.
    #[test]
    fn tag_validation_refuses_anything_that_could_steer_a_url() {
        assert!(validate_tag("v0.12.1").is_ok());
        for hostile in [
            "0.12.1",
            "v0.12.1/../../evil",
            "../v0.12.1",
            "v0.12.1?x=1",
            "https://evil.example/v1.0.0",
            "v0.12.1 ",
            "vlatest",
        ] {
            assert!(
                validate_tag(hostile).is_err(),
                "`{hostile}` must be refused"
            );
        }
    }

    /// The naming must match `release.yml`'s packaging step exactly.
    #[test]
    fn artifact_naming_matches_the_release_workflow() {
        let artifact = Artifact::new("v0.13.0", "aarch64-apple-darwin");
        assert_eq!(artifact.stage, "phux-v0.13.0-aarch64-apple-darwin");
        assert_eq!(artifact.archive, "phux-v0.13.0-aarch64-apple-darwin.tar.gz");
        assert_eq!(
            artifact.archive_url,
            "https://github.com/no-phux/phux/releases/download/v0.13.0/\
             phux-v0.13.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            artifact.checksum_url,
            format!("{}.sha256", artifact.archive_url)
        );
    }

    /// The list mixes every release stream; only a published core tag wins,
    /// even when another stream shipped newer.
    #[test]
    fn release_list_names_the_newest_core_release() {
        let body = r#"[
            {"tag_name":"cockpit-v0.20.0","draft":false,"prerelease":false},
            {"tag_name":"v0.31.0","draft":false,"prerelease":false},
            {"tag_name":"v0.30.0","draft":false,"prerelease":false}
        ]"#;
        assert_eq!(latest_core_tag_from_list(body).as_deref(), Some("v0.31.0"));
    }

    #[test]
    fn release_list_skips_drafts_prereleases_and_other_streams() {
        let body = r#"[
            {"tag_name":"v0.32.0","draft":true,"prerelease":false},
            {"tag_name":"v0.33.0-rc.1","draft":false,"prerelease":true},
            {"tag_name":"opencode-plugin-v0.2.2","draft":false,"prerelease":false},
            {"tag_name":"v0.31.0","draft":false,"prerelease":false}
        ]"#;
        assert_eq!(latest_core_tag_from_list(body).as_deref(), Some("v0.31.0"));
        assert!(latest_core_tag_from_list("not json").is_none());
        assert!(latest_core_tag_from_list(r#"[{"name":"x"}]"#).is_none());
        assert!(latest_core_tag_from_list("[]").is_none());
    }

    #[test]
    fn mixed_stream_fixture_matches_the_standalone_installers() {
        let body = include_str!("../../../../../scripts/fixtures/install-releases/mixed.json");
        assert_eq!(latest_core_tag_from_list(body).as_deref(), Some("v9.8.7"));
    }

    #[test]
    fn release_discovery_searches_later_pages_and_stops_at_the_match() {
        let mut calls = Vec::new();
        let tag = resolve_core_tag(|page| {
            calls.push(page);
            match page {
                1 => Ok(
                    r#"[{"tag_name":"cockpit-v9.8.7","draft":false,"prerelease":false}]"#
                        .to_owned(),
                ),
                2 => Ok(r#"[{"tag_name":"v9.8.7","draft":false,"prerelease":false}]"#.to_owned()),
                _ => panic!("must stop after finding the stable core tag"),
            }
        })
        .unwrap();
        assert_eq!(tag, "v9.8.7");
        assert_eq!(calls, [1, 2]);
    }

    #[test]
    fn release_discovery_bounds_exhaustion_and_stops_at_empty_pages() {
        let mut calls = 0;
        let err = resolve_core_tag(|_| {
            calls += 1;
            Ok(r#"[{"tag_name":"other-v1.0.0","draft":false,"prerelease":false}]"#.to_owned())
        })
        .unwrap_err();
        assert_eq!(calls, MAX_RELEASE_PAGES);
        assert!(err.to_string().contains("10 pages"));
        assert!(err.to_string().contains("--version"));
        calls = 0;
        let err = resolve_core_tag(|_| {
            calls += 1;
            Ok("[]".to_owned())
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(err.to_string().contains("--version"));
    }

    #[test]
    fn release_discovery_rejects_malformed_metadata_and_oversized_pages() {
        for body in [
            "not json",
            "{}",
            r#"[{"tag_name":"v1.2.3"}]"#,
            r#"[{"tag_name":"v1.2.3","draft":"false","prerelease":false}]"#,
            r#"[{"tag_name":"v1.2.3","draft":false,"draft":true,"prerelease":false}]"#,
            r#"[{"tag_name":"v1.2.3","draft":false,"prerelease":false}] garbage"#,
        ] {
            let err = resolve_core_tag(|page| {
                assert_eq!(page, 1);
                Ok(body.to_owned())
            })
            .unwrap_err();
            assert!(err.to_string().contains("invalid release list"), "{err}");
        }
        let body = " ".repeat(usize::try_from(MAX_RELEASE_PAGE_BYTES).unwrap() + 1);
        assert!(
            release_list(&body)
                .unwrap_err()
                .to_string()
                .contains("1048576")
        );
    }

    #[test]
    fn index_reader_kills_and_reaps_an_oversized_stream() {
        let err = run_index("cat", &["/dev/zero"]).unwrap_err();
        assert!(err.to_string().contains("1048576"));
        assert!(err.to_string().contains("--version"));
    }
}
