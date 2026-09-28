//! The release artifact contract `phux update` consumes (mirroring
//! `.github/workflows/release.yml` and `docs/RELEASING.md`), and
//! [`ReleaseSource`], the one network seam, so everything else is tested
//! against a local fake.

use std::path::Path;
use std::process::Command;

use super::UpdateError;

/// The repository releases are published from.
pub(crate) const REPO: &str = "no-phux/phux";

/// The redirect that names the current stable release (as
/// `scripts/install.sh` resolves it: not rate-limited, and a URL rather than a
/// JSON document).
const LATEST_REDIRECT: &str = "https://github.com/no-phux/phux/releases/latest";

/// Pointer file for the moving `next` prerelease (ADR-0113).
///
/// Hardcoded: a network-supplied tag is never interpolated into a URL.
pub(crate) const NEXT_CHANNEL_URL: &str =
    "https://github.com/no-phux/phux/releases/download/next/channel.json";

/// GitHub release tag the next channel publishes onto.
pub(crate) const NEXT_RELEASE_TAG: &str = "next";

/// A parsed `MAJOR.MINOR.PATCH`. Pre-release and build suffixes are refused,
/// not dropped, so two different releases never compare equal.
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
    /// Parse `X.Y.Z` or `vX.Y.Z`, strictly (no whitespace, no suffix): this
    /// feeds [`validate_tag`], which gates URL interpolation.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let body = text.strip_prefix('v').unwrap_or(text);
        let mut fields = body.split('.');
        let major = fields.next()?.parse().ok()?;
        let minor = fields.next()?.parse().ok()?;
        let patch = fields.next()?.parse().ok()?;
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

/// Validate a release tag (from `--version` or a redirect) before it is
/// interpolated into a URL: the exact `vX.Y.Z` shape rules out `../`, query
/// strings, and schemes.
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

/// The target triple this build's release artifact is published under (the
/// `release.yml` matrix; macOS `x86_64` is deliberately absent).
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
        Self::published(tag, format!("phux-{tag}-{target}"))
    }

    /// Next-channel artifact. The GitHub tag is always `next`; the SHA
    /// lives only in the filename, after [`validate_next_sha`] has accepted it.
    pub(crate) fn next(sha: &str, target: &str) -> Self {
        Self::published(NEXT_RELEASE_TAG, format!("phux-next.{sha}-{target}"))
    }

    fn published(release_tag: &str, stage: String) -> Self {
        let archive = format!("{stage}.tar.gz");
        let archive_url =
            format!("https://github.com/{REPO}/releases/download/{release_tag}/{archive}");
        let checksum_url = format!("{archive_url}.sha256");
        Self {
            archive,
            stage,
            archive_url,
            checksum_url,
        }
    }
}

/// Head of the `next` channel, parsed from `channel.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextHead {
    /// Full git SHA the artifacts were built from.
    pub(crate) sha: String,
    /// Cargo version baked into that build, informational.
    pub(crate) version: Option<String>,
}

/// A SHA interpolated into an artifact filename must be exactly 40 hex.
pub(crate) fn validate_next_sha(sha: &str) -> Result<(), UpdateError> {
    if sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(UpdateError::Fetch(
            "the next channel pointer named a SHA that is not 40 hex characters".to_owned(),
        ))
    }
}

/// Parse `channel.json`. Archive names in the document are ignored: URLs
/// are derived from the SHA so a poisoned pointer cannot steer the
/// download at an arbitrary path.
pub(crate) fn parse_next_channel(body: &str) -> Result<NextHead, UpdateError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|err| {
        UpdateError::Fetch(format!("the next channel pointer was not JSON: {err}"))
    })?;
    let schema = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if schema != Some(1) {
        return Err(UpdateError::Fetch(
            "the next channel pointer has an unsupported schema_version".to_owned(),
        ));
    }
    let channel = value.get("channel").and_then(serde_json::Value::as_str);
    if channel != Some("next") {
        return Err(UpdateError::Fetch(
            "the next channel pointer did not name channel `next`".to_owned(),
        ));
    }
    let sha = value
        .get("sha")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| UpdateError::Fetch("the next channel pointer named no sha".to_owned()))?
        .to_owned();
    validate_next_sha(&sha)?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_str)
        .and_then(Version::parse)
        .map(|version| version.to_string());
    Ok(NextHead { sha, version })
}

/// The one boundary that talks to the network. Deliberately dumb: every
/// decision happens on the near side, so a fake exercises the real code.
pub(crate) trait ReleaseSource: std::fmt::Debug {
    /// The tag of the current stable release (`vX.Y.Z`).
    fn latest_tag(&self) -> Result<String, UpdateError>;

    /// Head of the moving `next` prerelease.
    fn next_head(&self) -> Result<NextHead, UpdateError>;

    /// Download `url` into `dest`, replacing whatever is there.
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// The real [`ReleaseSource`]: `curl`, falling back to `wget`, which the
/// documented install already requires. The trust anchor is the checksum
/// verified afterwards, not the fetcher.
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
        if matches!(downloader, Downloader::Curl) {
            // The redirect's final path segment is the tag, but only when it
            // names the core stream; another stream falls through to the list.
            let out = run(
                "curl",
                &[
                    "-fsSLI",
                    "-o",
                    "/dev/null",
                    "-w",
                    "%{url_effective}",
                    LATEST_REDIRECT,
                ],
            )?;
            let tag = out.rsplit('/').next().unwrap_or_default().trim().to_owned();
            if tag.starts_with('v') && Version::parse(&tag).is_some() {
                return Ok(tag);
            }
        }
        // wget cannot print the effective URL, and the redirect may name
        // another stream anyway: list and take the newest core tag.
        let list_url = format!("https://api.github.com/repos/{REPO}/releases?per_page=30");
        let body = match downloader {
            Downloader::Curl => run("curl", &["-fsSL", &list_url])?,
            Downloader::Wget => run("wget", &["-q", "-O", "-", &list_url])?,
        };
        latest_core_tag_from_list(&body).ok_or_else(|| {
            UpdateError::Fetch("the GitHub releases list named no core phux release".to_owned())
        })
    }

    fn next_head(&self) -> Result<NextHead, UpdateError> {
        let downloader = Downloader::detect()?;
        let body = match downloader {
            Downloader::Curl => run("curl", &["-fsSL", NEXT_CHANNEL_URL])?,
            Downloader::Wget => run("wget", &["-q", "-O", "-", NEXT_CHANNEL_URL])?,
        };
        parse_next_channel(&body)
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

/// Name the newest core release (`vX.Y.Z`, not a draft or prerelease) in a
/// newest-first GitHub releases list; other streams are skipped.
fn latest_core_tag_from_list(body: &str) -> Option<String> {
    let releases: Vec<serde_json::Value> = serde_json::from_str(body).ok()?;
    releases.into_iter().find_map(|release| {
        if release.get("draft").and_then(serde_json::Value::as_bool) == Some(true) {
            return None;
        }
        if release
            .get("prerelease")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            return None;
        }
        let tag = release.get("tag_name")?.as_str()?;
        if !tag.starts_with('v') || Version::parse(tag).is_none() {
            return None;
        }
        Some(tag.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Artifact, Version, latest_core_tag_from_list, parse_next_channel, validate_next_sha,
        validate_tag,
    };

    #[test]
    fn versions_parse_order_and_refuse_unpublished_shapes() {
        assert_eq!(Version::parse("0.12.1"), Version::parse("v0.12.1"));
        let new = Version::parse("v0.13.0").expect("parses");
        assert!(new > Version::parse("v0.12.1").expect("parses"));
        assert_eq!(new.to_string(), "0.13.0");
        for text in ["", "v1", "1.2", "1.2.3.4", "1.2.x", "v1.2.3-rc.1", "latest"] {
            assert!(Version::parse(text).is_none(), "{text} should not parse");
        }
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

    #[test]
    fn next_artifact_urls_use_the_next_tag_and_the_sha_in_the_filename() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let artifact = Artifact::next(sha, "aarch64-apple-darwin");
        assert_eq!(
            artifact.stage,
            format!("phux-next.{sha}-aarch64-apple-darwin")
        );
        assert_eq!(
            artifact.archive_url,
            format!(
                "https://github.com/no-phux/phux/releases/download/next/\
                 phux-next.{sha}-aarch64-apple-darwin.tar.gz"
            )
        );
        assert_eq!(
            artifact.checksum_url,
            format!("{}.sha256", artifact.archive_url)
        );
    }

    #[test]
    fn next_channel_pointer_is_accepted_only_when_the_sha_is_40_hex() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let body =
            format!(r#"{{"schema_version":1,"channel":"next","sha":"{sha}","version":"0.32.0"}}"#);
        let head = parse_next_channel(&body).expect("valid pointer");
        assert_eq!(head.sha, sha);
        assert_eq!(head.version.as_deref(), Some("0.32.0"));
        assert!(validate_next_sha(sha).is_ok());
        assert!(validate_next_sha("abc").is_err());
        assert!(parse_next_channel(r#"{"schema_version":2,"channel":"next","sha":"0123456789abcdef0123456789abcdef01234567"}"#).is_err());
        assert!(parse_next_channel(r#"{"schema_version":1,"channel":"stable","sha":"0123456789abcdef0123456789abcdef01234567"}"#).is_err());
        assert!(
            parse_next_channel(r#"{"schema_version":1,"channel":"next","sha":"../evil"}"#).is_err()
        );
    }

    /// Only a published core tag wins, even when another stream shipped newer.
    #[test]
    fn release_list_names_the_newest_core_release() {
        let body = r#"[
            {"tag_name":"cockpit-v0.20.0","draft":false,"prerelease":false},
            {"tag_name":"v0.32.0","draft":true,"prerelease":false},
            {"tag_name":"v0.33.0-rc.1","draft":false,"prerelease":true},
            {"tag_name":"opencode-plugin-v0.2.2","draft":false,"prerelease":false},
            {"tag_name":"v0.31.0","draft":false,"prerelease":false},
            {"tag_name":"v0.30.0","draft":false,"prerelease":false}
        ]"#;
        assert_eq!(latest_core_tag_from_list(body).as_deref(), Some("v0.31.0"));
        assert!(latest_core_tag_from_list("not json").is_none());
        assert!(latest_core_tag_from_list(r#"[{"name":"x"}]"#).is_none());
        assert!(latest_core_tag_from_list("[]").is_none());
    }
}
