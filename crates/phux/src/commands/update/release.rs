//! The release artifact contract `phux update` consumes (mirroring
//! `.github/workflows/release.yml`, `docs/RELEASING.md`, and the channel
//! manifest of ADR-0175), and [`ReleaseSource`], the one network seam, so
//! everything else is tested against a local fake.

use std::path::Path;
use std::process::Command;

use super::UpdateError;
use super::channel::Channel;

/// The repository releases are published from.
pub(crate) const REPO: &str = "no-phux/phux";

/// The manifest naming each channel's current release (ADR-0175).
///
/// Hardcoded per channel: nothing from the network is interpolated into it.
pub(crate) const fn manifest_url(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "https://github.com/no-phux/phux/releases/download/channels/stable.json",
        Channel::Next => "https://github.com/no-phux/phux/releases/download/channels/next.json",
    }
}

/// GitHub release tag the next channel publishes onto.
pub(crate) const NEXT_RELEASE_TAG: &str = "next";

/// A manifest is a few kilobytes; anything near this is not one.
const MANIFEST_LIMIT_BYTES: &str = "1048576";

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
    /// lives only in the filename, after [`parse_manifest`] has accepted it.
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

/// The phux release a channel manifest names for this host, after every
/// field the manifest states has been checked against what this client
/// derives itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Published {
    /// The release's version.
    pub(crate) version: Version,
    /// `vX.Y.Z` on stable, `next.<sha>` on next.
    pub(crate) tag: String,
    /// The git commit the release was built from.
    pub(crate) sha: String,
    /// The protocol `major.minor` the release speaks (ADR-0061).
    pub(crate) protocol: String,
    /// This host's artifact.
    pub(crate) artifact: Artifact,
    /// The digest the manifest publishes for that artifact.
    pub(crate) sha256: String,
}

/// Exactly `len` lowercase hex digits.
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A decimal field with no leading zeroes.
fn is_canonical_number(field: &str) -> bool {
    field
        .parse::<u64>()
        .is_ok_and(|number| number.to_string() == field)
}

/// `major.minor`, decimal, no leading zeroes.
fn is_protocol(text: &str) -> bool {
    text.split_once('.')
        .is_some_and(|(major, minor)| is_canonical_number(major) && is_canonical_number(minor))
}

fn invalid(channel: Channel, detail: &str) -> UpdateError {
    UpdateError::Fetch(format!(
        "the {} release manifest is invalid: {detail}",
        channel.as_str()
    ))
}

/// A string member of `value`, or a manifest error naming it.
fn text<'a>(
    value: &'a serde_json::Value,
    key: &str,
    channel: Channel,
) -> Result<&'a str, UpdateError> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| invalid(channel, &format!("`{key}` is missing")))
}

/// Refuse a stated value that differs from the one this client derived.
fn expect_derived(
    stated: &str,
    derived: &str,
    what: &str,
    channel: Channel,
) -> Result<(), UpdateError> {
    if stated == derived {
        Ok(())
    } else {
        Err(invalid(channel, &format!("{what} is not `{derived}`")))
    }
}

/// The manifest's `products.phux`, after the document-level checks.
fn phux_entry(
    value: &serde_json::Value,
    channel: Channel,
) -> Result<&serde_json::Value, UpdateError> {
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(invalid(channel, "unsupported schema_version"));
    }
    expect_derived(
        text(value, "channel", channel)?,
        channel.as_str(),
        "`channel`",
        channel,
    )?;
    value
        .get("products")
        .and_then(|products| products.get("phux"))
        .ok_or_else(|| {
            UpdateError::Fetch(format!(
                "no phux release is published on the {} channel yet",
                channel.as_str()
            ))
        })
}

/// Parse a channel manifest (ADR-0175) into this host's phux release.
///
/// The manifest's `release`, artifact `name`, and `url` must equal what this
/// client derives from the version or SHA and `target`; a manifest that
/// disagrees is refused, so no network string ever steers a download.
pub(crate) fn parse_manifest(
    body: &str,
    channel: Channel,
    target: &str,
) -> Result<Published, UpdateError> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|err| invalid(channel, &format!("not JSON: {err}")))?;
    let phux = phux_entry(&value, channel)?;

    let version_text = text(phux, "version", channel)?;
    let version = Version::parse(version_text)
        .filter(|version| version.to_string() == version_text)
        .ok_or_else(|| invalid(channel, "`version` is not MAJOR.MINOR.PATCH"))?;
    let sha = text(phux, "sha", channel)?;
    if !is_lower_hex(sha, 40) {
        return Err(invalid(channel, "`sha` is not 40 lowercase hex"));
    }
    let protocol = text(phux, "protocol", channel)?;
    if !is_protocol(protocol) {
        return Err(invalid(channel, "`protocol` is not MAJOR.MINOR"));
    }
    let (release, tag, artifact) = match channel {
        Channel::Stable => {
            let tag = format!("v{version}");
            let artifact = Artifact::new(&tag, target);
            (tag.clone(), tag, artifact)
        }
        Channel::Next => (
            NEXT_RELEASE_TAG.to_owned(),
            format!("next.{sha}"),
            Artifact::next(sha, target),
        ),
    };
    expect_derived(
        text(phux, "release", channel)?,
        &release,
        "`release`",
        channel,
    )?;

    let entry = phux
        .get("artifacts")
        .and_then(|artifacts| artifacts.get(target))
        .ok_or_else(|| {
            UpdateError::UnsupportedPlatform(format!(
                "the {} release names no artifact for {target}",
                channel.as_str()
            ))
        })?;
    let what = format!("the {target} artifact");
    expect_derived(
        text(entry, "name", channel)?,
        &artifact.archive,
        &format!("{what} name"),
        channel,
    )?;
    expect_derived(
        text(entry, "url", channel)?,
        &artifact.archive_url,
        &format!("{what} url"),
        channel,
    )?;
    let sha256 = text(entry, "sha256", channel)?;
    if !is_lower_hex(sha256, 64) {
        return Err(invalid(channel, "`sha256` is not 64 lowercase hex"));
    }
    Ok(Published {
        version,
        tag,
        sha: sha.to_owned(),
        protocol: protocol.to_owned(),
        artifact,
        sha256: sha256.to_owned(),
    })
}

/// The one boundary that talks to the network. Deliberately dumb: every
/// decision happens on the near side, so a fake exercises the real code.
pub(crate) trait ReleaseSource: std::fmt::Debug {
    /// The body of `channel`'s release manifest.
    fn manifest(&self, channel: Channel) -> Result<String, UpdateError>;

    /// Download `url` into `dest`, replacing whatever is there.
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// The real [`ReleaseSource`]: `curl`, falling back to `wget`, which the
/// documented install already requires. The trust anchor is the digest
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
    fn manifest(&self, channel: Channel) -> Result<String, UpdateError> {
        let url = manifest_url(channel);
        let fetched = match Downloader::detect()? {
            Downloader::Curl => run(
                "curl",
                &[
                    "-fsSL",
                    "--proto",
                    "=https",
                    "--connect-timeout",
                    "10",
                    "--max-time",
                    "30",
                    "--max-filesize",
                    MANIFEST_LIMIT_BYTES,
                    url,
                ],
            ),
            Downloader::Wget => run("wget", &["-q", "--timeout=30", "--tries=2", "-O", "-", url]),
        };
        fetched.map_err(|err| match err {
            UpdateError::Fetch(detail) if detail.contains("404") => UpdateError::Fetch(format!(
                "the {} release manifest is not available ({url}); it is replaced \
                 in place when a release publishes, so retry in a minute",
                channel.as_str()
            )),
            other => other,
        })
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        let dest_arg = dest.to_string_lossy().into_owned();
        // A stalled transfer aborts after a minute under 1 KiB/s instead of
        // hanging; a slow transfer that keeps moving may finish.
        let fetched = match Downloader::detect()? {
            Downloader::Curl => run(
                "curl",
                &[
                    "-fsSL",
                    "--proto",
                    "=https",
                    "--connect-timeout",
                    "10",
                    "--speed-limit",
                    "1024",
                    "--speed-time",
                    "60",
                    url,
                    "-o",
                    &dest_arg,
                ],
            ),
            Downloader::Wget => run(
                "wget",
                &["-q", "--timeout=60", "--tries=2", "-O", &dest_arg, url],
            ),
        };
        fetched.map(|_| ())
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

#[cfg(test)]
mod tests {
    use super::{Artifact, Channel, Version, parse_manifest, validate_tag};

    /// The golden manifests every client parses (ADR-0175), rendered by
    /// `scripts/ci/release_manifest.py fixtures`.
    const STABLE: &str =
        include_str!("../../../../../scripts/fixtures/release-manifest/stable.json");
    const NEXT: &str = include_str!("../../../../../scripts/fixtures/release-manifest/next.json");
    const TARGET: &str = "x86_64-unknown-linux-gnu";

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
    }

    #[test]
    fn the_golden_manifests_parse_for_every_published_target() {
        for target in [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
        ] {
            let stable = parse_manifest(STABLE, Channel::Stable, target).expect("stable");
            assert_eq!(stable.tag, "v9.8.7");
            assert_eq!(stable.version.to_string(), "9.8.7");
            assert_eq!(stable.protocol, "0.9");
            assert_eq!(stable.artifact, Artifact::new("v9.8.7", target));
            assert_eq!(stable.sha256.len(), 64);

            let next = parse_manifest(NEXT, Channel::Next, target).expect("next");
            assert_eq!(next.tag, format!("next.{}", "a".repeat(40)));
            assert_eq!(next.artifact, Artifact::next(&"a".repeat(40), target));
        }
    }

    /// Each field a client derives itself is refused when the manifest
    /// states anything else, and each malformed field is refused.
    #[test]
    fn a_manifest_that_disagrees_with_the_derived_artifact_is_refused() {
        let url = "https://github.com/no-phux/phux/releases/download/v9.8.7/\
                   phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz";
        let cases = [
            ("\"schema_version\": 1", "\"schema_version\": 2"),
            ("\"channel\": \"stable\"", "\"channel\": \"next\""),
            ("\"version\": \"9.8.7\"", "\"version\": \"9.8.07\""),
            ("\"release\": \"v9.8.7\"", "\"release\": \"v9.8.6\""),
            (
                "\"protocol\": \"0.9\",\n      \"artifacts\"",
                "\"protocol\": \"0.09\",\n      \"artifacts\"",
            ),
            (
                url,
                "https://evil.example/phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz",
            ),
            (
                "\"name\": \"phux-v9.8.7-x86_64-unknown-linux-gnu.tar.gz\"",
                "\"name\": \"phux-v9.8.7-x86_64-unknown-linux-gnu.tgz\"",
            ),
            (&"2".repeat(64), &"2".repeat(63)),
            (&"a".repeat(40), &"A".repeat(40)),
        ];
        for (from, to) in cases {
            assert!(STABLE.contains(from), "fixture lacks {from}");
            let tampered = STABLE.replacen(from, to, 1);
            let err = parse_manifest(&tampered, Channel::Stable, TARGET)
                .expect_err(&format!("{from} -> {to} must be refused"));
            assert!(err.to_string().contains("manifest"), "{err}");
        }
        assert!(parse_manifest("not json", Channel::Stable, TARGET).is_err());
    }

    #[test]
    fn a_channel_without_phux_or_this_target_says_so() {
        let empty = r#"{"schema_version": 1, "channel": "next", "products": {}}"#;
        let err = parse_manifest(empty, Channel::Next, TARGET).unwrap_err();
        assert!(
            err.to_string().contains("no phux release is published"),
            "{err}"
        );

        let err =
            parse_manifest(STABLE, Channel::Stable, "riscv64gc-unknown-linux-gnu").unwrap_err();
        assert!(err.to_string().contains("no artifact for"), "{err}");
    }
}
