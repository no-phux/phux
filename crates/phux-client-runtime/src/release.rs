//! The release model every client shares (ADR-0176): channels, builds, the
//! one asset name derivation, and the channel manifest.
//!
//! Names and URLs are derived from the product, the build, and the target,
//! never read from the network, so a manifest vouches for an asset only
//! through its digest. `tools/src/release/release-manifest.ts` writes the
//! manifest and `scripts/install.sh` derives the same names; the golden
//! manifests in `scripts/fixtures/release-manifest/` keep all three in step.
//! A digest is trusted only once [`signature`] verifies that a release key
//! signed it.

pub mod signature;

use std::fmt;

use serde_json::Value;

/// Where every release asset and channel manifest is published.
pub const RELEASES: &str = "https://github.com/no-phux/phux/releases/download";

/// `curl` arguments for fetching a manifest, before its URL: HTTPS only,
/// bounded in time, and capped far above a manifest's few kilobytes.
pub const MANIFEST_CURL_ARGS: [&str; 9] = [
    "-fsSL",
    "--proto",
    "=https",
    "--connect-timeout",
    "10",
    "--max-time",
    "30",
    "--max-filesize",
    "1048576",
];

/// A release channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Channel {
    /// Numbered releases.
    #[default]
    Stable,
    /// The moving prerelease of green `main`.
    Next,
}

impl Channel {
    /// The channel's name in manifests, flags, and the `.phux-channel` file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Next => "next",
        }
    }

    /// Parse a channel name; `latest` is an alias for stable.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "stable" | "latest" => Some(Self::Stable),
            "next" => Some(Self::Next),
            _ => None,
        }
    }
}

/// A released product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Product {
    /// `phux` and `phux-mcp`, one tarball per target.
    Phux,
    /// The macOS app and Linux desktop, one archive per target.
    Desktop,
}

impl Product {
    /// The product's key under the manifest's `products`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Phux => "phux",
            Self::Desktop => "desktop",
        }
    }
}

/// A strict `MAJOR.MINOR.PATCH`: decimal, no leading zeroes, no suffix, so
/// two different releases never compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parse `X.Y.Z`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut fields = text.split('.').map(canonical_number);
        let version = Self {
            major: fields.next()??,
            minor: fields.next()??,
            patch: fields.next()??,
        };
        fields.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One build on a channel: a stable version, or a commit on next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Build {
    /// A stable release.
    Stable(Version),
    /// A next build, by its 40-hex commit.
    Next(String),
}

impl Build {
    /// The channel this build is published on.
    #[must_use]
    pub const fn channel(&self) -> Channel {
        match self {
            Self::Stable(_) => Channel::Stable,
            Self::Next(_) => Channel::Next,
        }
    }

    /// The build's tag-like name: `vX.Y.Z` or `next.<sha>`.
    #[must_use]
    pub fn tag(&self) -> String {
        match self {
            Self::Stable(version) => format!("v{version}"),
            Self::Next(sha) => format!("next.{sha}"),
        }
    }
}

/// Whether moving from `installed` to `target` is an update: a channel
/// switch, another commit on next, or a higher version on stable.
#[must_use]
pub fn is_newer(installed: &Build, target: &Build) -> bool {
    match (installed, target) {
        (Build::Stable(installed), Build::Stable(target)) => target > installed,
        (Build::Next(installed), Build::Next(target)) => target != installed,
        _ => true,
    }
}

/// One downloadable release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// The file name without its extension; a phux tarball's members all
    /// sit under a directory of this name.
    pub stem: String,
    /// The file name.
    pub name: String,
    /// Where it is downloaded from.
    pub url: String,
}

impl Asset {
    /// The `sha256sum`-format sidecar published beside every asset.
    #[must_use]
    pub fn sidecar_url(&self) -> String {
        format!("{}.sha256", self.url)
    }

    /// The minisign signature over the sidecar (ADR-0178).
    #[must_use]
    pub fn signature_url(&self) -> String {
        format!("{}.sha256.minisig", self.url)
    }
}

/// The asset `product` publishes for `build` on `target`: the one name
/// derivation. Linux desktop archives use short architecture labels; macOS
/// keeps its original Apple silicon name.
#[must_use]
pub fn asset(product: Product, build: &Build, target: &str) -> Asset {
    let (desktop_platform, desktop_extension) = desktop_archive(target);
    let (release, stem, extension) = match (product, build) {
        (Product::Phux, Build::Stable(version)) => (
            format!("v{version}"),
            format!("phux-v{version}-{target}"),
            "tar.gz",
        ),
        (Product::Phux, Build::Next(sha)) => (
            "next".to_owned(),
            format!("phux-next.{sha}-{target}"),
            "tar.gz",
        ),
        (Product::Desktop, Build::Stable(version)) => (
            format!("desktop-v{version}"),
            format!("phux-desktop-{version}-{desktop_platform}"),
            desktop_extension,
        ),
        (Product::Desktop, Build::Next(sha)) => (
            "next".to_owned(),
            format!("phux-desktop-next.{sha}-{desktop_platform}"),
            desktop_extension,
        ),
    };
    let name = format!("{stem}.{extension}");
    let url = format!("{RELEASES}/{release}/{name}");
    Asset { stem, name, url }
}

fn desktop_archive(target: &str) -> (&'static str, &'static str) {
    match target {
        "x86_64-unknown-linux-gnu" => ("linux-x64", "tar.gz"),
        "aarch64-unknown-linux-gnu" => ("linux-arm64", "tar.gz"),
        _ => ("macos-arm64", "zip"),
    }
}

/// The URL of `channel`'s manifest.
#[must_use]
pub fn manifest_url(channel: Channel) -> String {
    format!("{RELEASES}/channels/{}.json", channel.as_str())
}

/// What a manifest publishes for one product on one target, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The build: the version on stable, the commit on next.
    pub build: Build,
    /// The release version.
    pub version: Version,
    /// The commit it was built from.
    pub sha: String,
    /// The protocol `major.minor` it speaks (ADR-0061).
    pub protocol: String,
    /// The target asset's SHA-256, lowercase hex.
    pub sha256: String,
}

/// A manifest a client must not trust.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the {} release manifest is invalid: {detail}", channel.as_str())]
pub struct ManifestError {
    channel: Channel,
    detail: String,
}

/// Parse `channel`'s manifest for `product` on `target`. `Ok(None)` means the
/// product is not published on that channel. Unknown members are ignored.
///
/// # Errors
///
/// A body that is not a schema 1 manifest for `channel`, or whose entry for
/// `product` has a malformed field or no digest for `target`.
pub fn parse(
    body: &str,
    channel: Channel,
    product: Product,
    target: &str,
) -> Result<Option<Entry>, ManifestError> {
    let invalid = |detail: String| ManifestError { channel, detail };
    let document: Value =
        serde_json::from_str(body).map_err(|err| invalid(format!("not JSON: {err}")))?;
    if document["schema_version"] != 1 {
        return Err(invalid("unsupported `schema_version`".to_owned()));
    }
    if document["channel"] != channel.as_str() {
        return Err(invalid(format!("`channel` is not `{}`", channel.as_str())));
    }
    let entry = &document["products"][product.as_str()];
    if entry.is_null() {
        return Ok(None);
    }
    let field = |key: &str| {
        entry[key]
            .as_str()
            .ok_or_else(|| invalid(format!("{} `{key}` is missing", product.as_str())))
    };
    let malformed = |key: &str| invalid(format!("{} `{key}` is malformed", product.as_str()));
    let version = Version::parse(field("version")?).ok_or_else(|| malformed("version"))?;
    let sha = field("sha")?;
    if !is_lower_hex(sha, 40) {
        return Err(malformed("sha"));
    }
    let protocol = field("protocol")?;
    if !is_protocol(protocol) {
        return Err(malformed("protocol"));
    }
    let sha256 = entry["artifacts"][target]["sha256"]
        .as_str()
        .filter(|digest| is_lower_hex(digest, 64))
        .ok_or_else(|| {
            invalid(format!(
                "{} has no valid digest for {target}",
                product.as_str()
            ))
        })?;
    let build = match channel {
        Channel::Stable => Build::Stable(version),
        Channel::Next => Build::Next(sha.to_owned()),
    };
    Ok(Some(Entry {
        build,
        version,
        sha: sha.to_owned(),
        protocol: protocol.to_owned(),
        sha256: sha256.to_owned(),
    }))
}

/// Exactly `len` lowercase hex digits.
#[must_use]
pub fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `major.minor`, decimal, no leading zeroes.
fn is_protocol(text: &str) -> bool {
    text.split_once('.').is_some_and(|(major, minor)| {
        canonical_number(major).is_some() && canonical_number(minor).is_some()
    })
}

/// A decimal field with no sign and no leading zeroes.
fn canonical_number(field: &str) -> Option<u64> {
    let number = field.parse::<u64>().ok()?;
    (number.to_string() == field).then_some(number)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::{Build, Channel, Product, Version, asset, is_newer, manifest_url, parse};

    const STABLE: &str = include_str!("../../../scripts/fixtures/release-manifest/stable.json");
    const NEXT: &str = include_str!("../../../scripts/fixtures/release-manifest/next.json");
    const LINUX: &str = "x86_64-unknown-linux-gnu";

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_are_strict_and_ordered() {
        assert!(version("0.13.0") > version("0.12.10"));
        assert_eq!(version("10.0.1").to_string(), "10.0.1");
        for text in [
            "",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.2.x",
            "1.2.3-rc.1",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn channels_and_manifest_urls() {
        assert_eq!(Channel::parse("latest"), Some(Channel::Stable));
        assert_eq!(Channel::parse("next"), Some(Channel::Next));
        assert_eq!(Channel::parse("nightly"), None);
        assert_eq!(
            manifest_url(Channel::Next),
            "https://github.com/no-phux/phux/releases/download/channels/next.json"
        );
    }

    /// ADR-0176 decision 1, row by row.
    #[test]
    fn asset_names_follow_the_one_table() {
        let sha = "a".repeat(40);
        let stable = Build::Stable(version("1.2.3"));
        let next = Build::Next(sha.clone());
        let rows = [
            (
                Product::Phux,
                &stable,
                "v1.2.3/phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
            ),
            (
                Product::Phux,
                &next,
                format!("next/phux-next.{sha}-x86_64-unknown-linux-gnu.tar.gz"),
            ),
            (
                Product::Desktop,
                &stable,
                "desktop-v1.2.3/phux-desktop-1.2.3-linux-x64.tar.gz".to_owned(),
            ),
            (
                Product::Desktop,
                &next,
                format!("next/phux-desktop-next.{sha}-linux-x64.tar.gz"),
            ),
        ];
        for (product, build, path) in rows {
            let asset = asset(product, build, LINUX);
            assert_eq!(
                asset.url,
                format!("https://github.com/no-phux/phux/releases/download/{path}")
            );
            assert!(path.ends_with(&format!("/{}", asset.name)));
            assert!(asset.name.starts_with(&asset.stem));
            assert_eq!(asset.sidecar_url(), format!("{}.sha256", asset.url));
            assert_eq!(
                asset.signature_url(),
                format!("{}.sha256.minisig", asset.url)
            );
        }
    }

    #[test]
    fn desktop_archives_cover_every_published_target() {
        let stable = Build::Stable(version("1.2.3"));
        let next = Build::Next("b".repeat(40));
        for (target, suffix) in [
            ("aarch64-apple-darwin", "macos-arm64.zip"),
            (LINUX, "linux-x64.tar.gz"),
            ("aarch64-unknown-linux-gnu", "linux-arm64.tar.gz"),
        ] {
            for (build, identity) in [
                (&stable, "1.2.3".to_owned()),
                (&next, format!("next.{}", "b".repeat(40))),
            ] {
                let archive = asset(Product::Desktop, build, target);
                assert_eq!(archive.name, format!("phux-desktop-{identity}-{suffix}"));
                let body = match build.channel() {
                    Channel::Stable => STABLE,
                    Channel::Next => NEXT,
                };
                let entry = parse(body, build.channel(), Product::Desktop, target)
                    .unwrap()
                    .unwrap();
                let document: serde_json::Value = serde_json::from_str(body).unwrap();
                let recorded = &document["products"]["desktop"]["artifacts"][target];
                assert_eq!(
                    recorded["name"],
                    asset(Product::Desktop, &entry.build, target).name
                );
                assert_eq!(entry.sha256.len(), 64);
            }
        }
    }

    #[test]
    fn an_update_is_a_switch_a_new_commit_or_a_higher_version() {
        let (old, new) = (
            Build::Stable(version("1.2.3")),
            Build::Stable(version("1.3.0")),
        );
        let (a, b) = (Build::Next("a".into()), Build::Next("b".into()));
        assert!(is_newer(&old, &new) && !is_newer(&new, &old) && !is_newer(&new, &new));
        assert!(is_newer(&a, &b) && !is_newer(&a, &a));
        assert!(is_newer(&a, &new) && is_newer(&new, &a));
    }

    #[test]
    fn the_golden_manifests_parse() {
        for (body, channel) in [(STABLE, Channel::Stable), (NEXT, Channel::Next)] {
            let phux = parse(body, channel, Product::Phux, LINUX).unwrap().unwrap();
            assert_eq!(
                (phux.version, phux.protocol.as_str()),
                (version("9.8.7"), "0.9")
            );
            assert_eq!(phux.sha256, "2".repeat(64));
            let desktop = parse(body, channel, Product::Desktop, "aarch64-apple-darwin")
                .unwrap()
                .unwrap();
            assert_eq!(
                (desktop.sha.as_str(), desktop.sha256.as_str()),
                (&*"b".repeat(40), &*"d".repeat(64))
            );
            assert_eq!(desktop.build.channel(), channel);
        }
        let next = parse(NEXT, Channel::Next, Product::Phux, LINUX)
            .unwrap()
            .unwrap();
        assert_eq!(next.build.tag(), format!("next.{}", "a".repeat(40)));
    }

    #[test]
    fn every_malformed_field_is_refused() {
        let cases = [
            ("\"schema_version\": 1", "\"schema_version\": 2"),
            ("\"channel\": \"stable\"", "\"channel\": \"next\""),
            ("\"version\": \"9.8.7\"", "\"version\": \"9.8.07\""),
            ("\"protocol\": \"0.9\"", "\"protocol\": \"0.09\""),
            (&"a".repeat(40), &"A".repeat(40)),
            (&"2".repeat(64), &"2".repeat(63)),
        ];
        for (from, to) in cases {
            assert!(STABLE.contains(from), "the fixture lacks {from}");
            let tampered = STABLE.replacen(from, to, 1);
            let err = parse(&tampered, Channel::Stable, Product::Phux, LINUX).unwrap_err();
            assert!(err.to_string().contains("stable release manifest"), "{err}");
        }
        assert!(parse("not json", Channel::Stable, Product::Phux, LINUX).is_err());
        assert!(
            parse(
                STABLE,
                Channel::Stable,
                Product::Phux,
                "riscv64gc-unknown-linux-gnu"
            )
            .is_err()
        );
    }

    #[test]
    fn an_absent_product_is_unpublished_and_extra_members_are_ignored() {
        let empty = r#"{"schema_version": 1, "channel": "next", "products": {}}"#;
        assert_eq!(
            parse(empty, Channel::Next, Product::Desktop, LINUX),
            Ok(None)
        );
        let additive = STABLE.replacen("\"protocol\"", "\"future\": \"x\",\n      \"protocol\"", 1);
        assert!(
            parse(&additive, Channel::Stable, Product::Phux, LINUX)
                .unwrap()
                .is_some()
        );
    }
}
