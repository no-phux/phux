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

use std::path::Path;
use std::process::Command;

use super::UpdateError;

/// The repository releases are published from.
pub(crate) const REPO: &str = "no-phux/phux";

/// The redirect that names the current stable release.
///
/// Resolving the redirect (rather than reading `api.github.com`) is what
/// `scripts/install.sh` already does: it is not rate-limited for anonymous
/// callers and the answer is a URL, not a JSON document that has to be
/// trusted and parsed.
const LATEST_REDIRECT: &str = "https://github.com/no-phux/phux/releases/latest";

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
        if matches!(downloader, Downloader::Curl) {
            // A HEAD that follows redirects and prints only the final URL.
            // The last path segment of `…/releases/tag/vX.Y.Z` is the tag —
            // but only when it names the core stream. The redirect follows
            // whichever stream shipped most recently, so a newer Cockpit (or
            // integration) release lands here as a tag this updater must not
            // touch; that falls through to the filtered list below.
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

/// Name the newest core release in a GitHub releases list document.
///
/// The repository publishes several streams (`vX.Y.Z` for the CLI,
/// `cockpit-vX.Y.Z`, integration packages) and only bare `vX.Y.Z` tags carry
/// the tarballs this updater installs. GitHub returns newest-first, so the
/// first entry that is neither draft nor prerelease and parses as a core tag
/// is the current release. Drafts and prereleases are skipped rather than
/// trusted: a draft has no assets and a prerelease is not what `update`
/// without `--version` should install.
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
    use super::{Artifact, Version, latest_core_tag_from_list, validate_tag};

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
        for text in ["", "v1", "1.2", "1.2.3.4", "1.2.x", "v1.2.3-rc.1", "latest"] {
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
}
