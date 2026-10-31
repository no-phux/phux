//! Release checks for this app (`rune-v*` prereleases), the same contract
//! as the GPUI desktop's: a check reads one unsigned manifest and runs
//! nothing it downloads. Asset names are derived here, never read from the
//! network, and a digest counts only once a release key signed it.

pub mod install;
pub mod phux;

use std::path::{Path, PathBuf};
use std::process::Command;

use phux_client_runtime::release::{MANIFEST_CURL_ARGS, RELEASES, is_lower_hex};
use serde_json::Value;

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const FEED: &str = "channels/rune.json";
const APP: &str = "Phux Rune.app";
const EXECUTABLE: &str = "phux-rune";
/// Beside a Linux binary: this is a packaged install, not a build tree.
const LINUX_MARKER: &str = "phux-rune.release";
const CURL: &str = "/usr/bin/curl";

const TARGET: &str = if cfg!(target_os = "macos") {
    "aarch64-apple-darwin"
} else if cfg!(target_arch = "x86_64") {
    "x86_64-unknown-linux-gnu"
} else {
    "unsupported"
};

/// `X.Y.Z` or `X.Y.Z-alpha.N`; a release sorts after its alphas, and nothing
/// else parses, so a version is safe in a URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    release: [u64; 3],
    alpha: u64,
}

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let number = |field: &str| {
            let plain = field.bytes().all(|b| b.is_ascii_digit());
            (plain && (field == "0" || !field.starts_with('0')))
                .then(|| field.parse().ok())
                .flatten()
        };
        let (numbers, alpha) = match text.split_once("-alpha.") {
            Some((numbers, alpha)) => (numbers, number(alpha)?),
            None => (text, u64::MAX),
        };
        let mut fields = numbers.split('.').map(number);
        let release = [fields.next()??, fields.next()??, fields.next()??];
        fields.next().is_none().then_some(Self { release, alpha })
    }
}

/// What the feed publishes for this computer, validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub sha256: String,
    /// The archive's size, for a real download percentage.
    pub size: u64,
    /// The wire `major.minor` it links.
    pub protocol: String,
}

impl Release {
    /// A higher version only: a stale or rewritten feed can't move us back.
    pub fn newer_than_installed(&self) -> bool {
        Version::parse(&self.version) > Version::parse(VERSION)
    }

    fn archive(&self) -> String {
        let platform = if cfg!(target_os = "macos") {
            "macos-arm64.zip"
        } else {
            "linux-x86_64.tar.gz"
        };
        format!("phux-rune-{}-{platform}", self.version)
    }

    fn url(&self) -> String {
        format!("{RELEASES}/rune-v{}/{}", self.version, self.archive())
    }
}

pub fn parse(body: &str, target: &str) -> Result<Release, String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| format!("The update feed is invalid: {error}"))?;
    if value["schema"] != 1 {
        return Err("The update feed has an unknown schema.".to_owned());
    }
    let text = |key: &str| value[key].as_str().unwrap_or_default();
    let version = text("version");
    if Version::parse(version).is_none() {
        return Err("The update feed names an invalid version.".to_owned());
    }
    let entry = &value["targets"][target];
    if entry.is_null() {
        return Err(format!("No build for this computer in {version}."));
    }
    let sha256 = entry["sha256"].as_str().unwrap_or_default();
    let size = entry["size"].as_u64().filter(|size| *size > 0);
    let (true, Some(size)) = (is_lower_hex(sha256, 64), size) else {
        return Err("The update feed has a malformed entry.".to_owned());
    };
    Ok(Release {
        version: version.to_owned(),
        sha256: sha256.to_owned(),
        size,
        protocol: text("protocol").to_owned(),
    })
}

/// One small release file over HTTPS.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let output = Command::new(CURL)
        .args(MANIFEST_CURL_ARGS)
        .arg(url)
        .output()
        .map_err(|error| format!("Could not start curl: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

pub fn latest() -> Result<Release, String> {
    let body = fetch(&format!("{RELEASES}/{FEED}"))?;
    parse(&String::from_utf8_lossy(&body), TARGET)
}

/// Where this copy may replace itself, or why it can't. Only a packaged
/// install qualifies, so a development build can't turn into an update.
pub fn installation(executable: &Path) -> Result<PathBuf, &'static str> {
    const DEVELOPMENT: &str = "Development build: updates are off.";
    if cfg!(target_os = "macos") {
        let layout = Path::new(APP).join("Contents/MacOS").join(EXECUTABLE);
        if !executable.ends_with(&layout) {
            return Err(DEVELOPMENT);
        }
        // Gatekeeper runs an app opened from Downloads from a read-only copy.
        if executable.to_string_lossy().contains("/AppTranslocation/") {
            return Err("Move Phux Rune to Applications to get updates.");
        }
        let bundle = executable.ancestors().nth(3).ok_or(DEVELOPMENT)?;
        return Ok(bundle.to_path_buf());
    }
    let dir = executable.parent().ok_or(DEVELOPMENT)?;
    if dir.join(LINUX_MARKER).is_file() {
        Ok(dir.to_path_buf())
    } else {
        Err(DEVELOPMENT)
    }
}

/// Whether two `major.minor` protocols interoperate: the major decides.
pub fn same_protocol_line(a: &str, b: &str) -> bool {
    a.split('.').next() == b.split('.').next()
}

/// The wire protocol this build links.
pub fn own_protocol() -> String {
    let version = phux_protocol::PROTOCOL_VERSION;
    format!("{}.{}", version.major, version.minor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_order_alphas_before_their_release() {
        assert!(v("0.1.0-alpha.5") < v("0.1.0-alpha.6"));
        assert!(v("0.1.0-alpha.9") < v("0.1.0-alpha.10"));
        assert!(v("0.1.0-alpha.10") < v("0.1.0"));
        assert!(v("0.1.0") < v("0.1.1-alpha.1"));
    }

    #[test]
    fn versions_refuse_anything_else() {
        for bad in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-beta.1",
            "1.2.3-alpha.",
            "1.2.3-alpha.01",
            "../1.2.3",
            "1.2.3/x",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    fn feed(version: &str, sha256: &str, size: u64) -> String {
        serde_json::json!({
            "schema": 1, "version": version, "protocol": "3.4",
            "targets": { "t": { "sha256": sha256, "size": size } },
        })
        .to_string()
    }

    #[test]
    fn feed_parses_and_derives_the_asset() {
        let sha = "a".repeat(64);
        let release = parse(&feed("0.1.0-alpha.9", &sha, 7), "t").unwrap();
        assert_eq!(release.size, 7);
        assert_eq!(release.protocol, "3.4");
        assert!(release.url().starts_with(
            "https://github.com/no-phux/phux/releases/download/rune-v0.1.0-alpha.9/phux-rune-0.1.0-alpha.9-"
        ));
    }

    #[test]
    fn feed_refuses_what_it_cannot_trust() {
        let sha = "a".repeat(64);
        assert!(parse("nope", "t").is_err());
        assert!(parse(&feed("../evil", &sha, 7), "t").is_err());
        assert!(parse(&feed("0.1.0", "ABC", 7), "t").is_err());
        assert!(parse(&feed("0.1.0", &sha, 0), "t").is_err());
        assert!(parse(&feed("0.1.0", &sha, 7), "other").is_err());
        assert!(parse(r#"{"schema":2}"#, "t").is_err());
    }

    #[test]
    fn only_a_higher_version_is_an_update() {
        let at = |version: &str| Release {
            version: version.to_owned(),
            sha256: String::new(),
            size: 1,
            protocol: String::new(),
        };
        assert!(at("99.0.0").newer_than_installed());
        assert!(!at(VERSION).newer_than_installed());
        assert!(!at("0.0.1").newer_than_installed());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn only_the_packaged_layout_updates() {
        let app = "/Applications/Phux Rune.app/Contents/MacOS/phux-rune";
        assert_eq!(
            installation(Path::new(app)),
            Ok(PathBuf::from("/Applications/Phux Rune.app"))
        );
        assert!(installation(Path::new("/w/target/release/phux-rune")).is_err());
        let moved = "/private/var/x/AppTranslocation/y/d/Phux Rune.app/Contents/MacOS/phux-rune";
        assert!(
            installation(Path::new(moved))
                .unwrap_err()
                .contains("Applications")
        );
    }

    #[test]
    fn protocol_lines_compare_by_major() {
        assert!(same_protocol_line("3.1", "3.9"));
        assert!(!same_protocol_line("3.1", "4.1"));
    }
}
