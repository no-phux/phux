//! What `phux update` needs beyond the shared release model
//! (`phux_client_runtime::release`): this host's target, `--version` pins,
//! and [`ReleaseSource`], the one network seam, so everything else is tested
//! against a local fake.

use std::path::Path;
use std::process::{Command, Output};

use phux_client_runtime::release::{self, Channel, Entry, MANIFEST_CURL_ARGS, Product, Version};

use super::UpdateError;

/// The version this binary was built as.
pub(crate) fn current_version() -> Result<Version, UpdateError> {
    Version::parse(env!("CARGO_PKG_VERSION")).ok_or_else(|| {
        UpdateError::Install(format!(
            "this build's version (`{}`) is not a release version",
            env!("CARGO_PKG_VERSION")
        ))
    })
}

/// Validate a `--version` tag before it names a download: exactly
/// `vX.Y.Z`, which rules out `../`, query strings, and schemes.
pub(crate) fn validate_tag(tag: &str) -> Result<Version, UpdateError> {
    tag.strip_prefix('v')
        .and_then(Version::parse)
        .ok_or_else(|| {
            UpdateError::InvalidTag(format!(
                "`{tag}` is not a release tag; releases are tagged vMAJOR.MINOR.PATCH"
            ))
        })
}

/// The target triple this host's release tarball is published under.
pub(crate) fn host_target() -> Result<&'static str, UpdateError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (os, arch) => Err(UpdateError::UnsupportedPlatform(format!(
            "{os}/{arch} has no published release artifact; build from source"
        ))),
    }
}

/// The phux release `channel`'s manifest names for `target`.
pub(crate) fn published(body: &str, channel: Channel, target: &str) -> Result<Entry, UpdateError> {
    release::parse(body, channel, Product::Phux, target)
        .map_err(|err| UpdateError::Fetch(err.to_string()))?
        .ok_or_else(|| {
            UpdateError::Fetch(format!(
                "no phux release is published on the {} channel yet",
                channel.as_str()
            ))
        })
}

/// The one boundary that talks to the network. Deliberately dumb: every
/// decision happens on the near side, so a fake exercises the real code.
pub(crate) trait ReleaseSource: std::fmt::Debug {
    /// The body of `channel`'s release manifest.
    fn manifest(&self, channel: Channel) -> Result<String, UpdateError>;

    /// The minisign signature over `channel`'s manifest
    /// (`<channel>.json.minisig`).
    fn manifest_signature(&self, channel: Channel) -> Result<String, UpdateError>;

    /// Download `url` into `dest`, replacing whatever is there.
    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// The real [`ReleaseSource`]: `curl`, or `wget` where curl is absent. The
/// digest verified afterwards is the trust anchor, not the fetcher. Each file
/// that `releases/download` will not serve is fetched once more through
/// GitHub's API ([`release::API_RELEASES`]).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct NetworkReleaseSource;

impl ReleaseSource for NetworkReleaseSource {
    fn manifest(&self, channel: Channel) -> Result<String, UpdateError> {
        let url = release::manifest_url(channel);
        with_api_fallback(&url, get_manifest).map_err(|err| match err {
            UpdateError::Fetch(detail) if detail.contains("404") => UpdateError::Fetch(format!(
                "the {} release manifest is not available ({url}); it is replaced in place \
                 when a release publishes, so retry in a minute",
                channel.as_str()
            )),
            other => other,
        })
    }

    fn manifest_signature(&self, channel: Channel) -> Result<String, UpdateError> {
        with_api_fallback(&release::manifest_signature_url(channel), get_manifest)
    }

    fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        let dest = dest.to_string_lossy();
        with_api_fallback(url, |url, accept| {
            // A transfer stalled under 1 KiB/s for a minute aborts; a slow one
            // that keeps moving finishes.
            let curl = [
                "-fsSL",
                "--proto",
                "=https",
                "--connect-timeout",
                "10",
                "--speed-limit",
                "1024",
                "--speed-time",
                "60",
                "--retry",
                "2",
            ];
            let wget = ["-q", "--timeout=60", "--tries=2", "-O", &dest];
            fetch(
                &with_header(&curl, accept, &[url, "-o", &dest]),
                &with_wget_header(&wget, accept, url),
            )
        })
        .map(drop)
    }
}

/// `curl`/`wget` for one manifest-sized file, sending `accept` when given.
fn get_manifest(url: &str, accept: Option<&str>) -> Result<String, UpdateError> {
    fetch(
        &with_header(&MANIFEST_CURL_ARGS, accept, &[url]),
        &with_wget_header(&["-q", "--timeout=30", "--tries=2", "-O", "-"], accept, url),
    )
}

/// Run `attempt` against `url`; when that fails and `url` is a release asset,
/// look the asset up through the API and attempt its API URL instead. The
/// first error is the one reported if the API cannot serve it either.
fn with_api_fallback(
    url: &str,
    attempt: impl Fn(&str, Option<&str>) -> Result<String, UpdateError>,
) -> Result<String, UpdateError> {
    let first = match attempt(url, None) {
        Ok(body) => return Ok(body),
        Err(err) => err,
    };
    let Some(api) = api_asset(url) else {
        return Err(first);
    };
    attempt(&api, Some(release::API_ASSET_ACCEPT)).map_err(|_| first)
}

/// The API URL serving the release asset at `url`, if the API lists one.
fn api_asset(url: &str) -> Option<String> {
    let (lookup, name) = release::api_release_lookup(url)?;
    let curl = [
        "-fsSL",
        "--proto",
        "=https",
        "--connect-timeout",
        "10",
        "--max-time",
        "30",
        "--max-filesize",
        "16777216",
        "--retry",
        "2",
    ];
    let accept = Some(release::API_JSON_ACCEPT);
    let wget = ["-q", "--timeout=30", "--tries=2", "-O", "-"];
    let body = fetch(
        &with_header(&curl, accept, &[&lookup]),
        &with_wget_header(&wget, accept, &lookup),
    )
    .ok()?;
    release::api_asset_url(&body, &name)
}

/// `args`, then `-H accept` when given, then `tail`.
fn with_header<'a>(args: &[&'a str], accept: Option<&'a str>, tail: &[&'a str]) -> Vec<&'a str> {
    let header = accept.into_iter().flat_map(|accept| ["-H", accept]);
    args.iter()
        .copied()
        .chain(header)
        .chain(tail.iter().copied())
        .collect()
}

/// wget `args`, then `--header=accept` when given, then `url`.
fn with_wget_header(args: &[&str], accept: Option<&str>, url: &str) -> Vec<String> {
    let header = accept.map(|accept| format!("--header={accept}"));
    args.iter()
        .map(|arg| (*arg).to_owned())
        .chain(header)
        .chain([url.to_owned()])
        .collect()
}

/// Run curl with `curl` arguments, or wget with `wget` arguments when curl is
/// not installed, returning stdout.
fn fetch(curl: &[&str], wget: &[String]) -> Result<String, UpdateError> {
    let wget: Vec<&str> = wget.iter().map(String::as_str).collect();
    for (program, args) in [("curl", curl), ("wget", wget.as_slice())] {
        match Command::new(program).args(args).output() {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(UpdateError::Fetch(format!(
                    "could not run `{program}`: {err}"
                )));
            }
            Ok(output) => return finished(program, &output),
        }
    }
    Err(UpdateError::Fetch(
        "neither `curl` nor `wget` is on PATH; one of them is required to download a release"
            .to_owned(),
    ))
}

fn finished(program: &str, output: &Output) -> Result<String, UpdateError> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = match stderr.trim() {
        "" => String::new(),
        detail => format!(": {detail}"),
    };
    Err(UpdateError::Fetch(format!(
        "`{program}` exited with {}{detail}",
        output.status
    )))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::validate_tag;

    /// A pin names a download URL, so anything that is not exactly `vX.Y.Z`
    /// is refused before it gets there.
    #[test]
    fn a_pin_that_could_steer_a_url_is_refused() {
        assert_eq!(validate_tag("v0.12.1").unwrap().to_string(), "0.12.1");
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
}
