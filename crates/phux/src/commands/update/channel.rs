//! Release channel: `stable` (vX.Y.Z) or `next` (green main).

use std::fs;
use std::path::Path;

use super::UpdateError;
use super::source::Install;

/// Persisted beside the binaries so the next `phux update` stays on the
/// rail the user opted into, even when `--channel` is omitted.
pub(crate) const CHANNEL_FILE: &str = ".phux-channel";

/// The two rails `phux update` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Channel {
    /// `vX.Y.Z` GitHub releases and `releases/latest`.
    Stable,
    /// Moving prerelease of green `main`.
    Next,
}

impl Channel {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Next => "next",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "stable" => Some(Self::Stable),
            "next" => Some(Self::Next),
            _ => None,
        }
    }

    /// The channel this binary was compiled as.
    pub(crate) fn from_build() -> Self {
        match option_env!("PHUX_BUILD_CHANNEL") {
            Some("next") => Self::Next,
            _ => Self::Stable,
        }
    }
}

/// Git SHA baked into a next build, if any.
pub(crate) const fn build_sha() -> Option<&'static str> {
    match option_env!("PHUX_BUILD_SHA") {
        Some(sha) if !sha.is_empty() => Some(sha),
        _ => None,
    }
}

/// `--channel` wins; then `<bindir>/.phux-channel`; then the baked-in
/// channel so a next binary stays on next after the first install.
pub(crate) fn resolve(explicit: Option<Channel>, install: &Install) -> Channel {
    if let Some(channel) = explicit {
        return channel;
    }
    if let Some(dir) = install.bin_dir()
        && let Some(channel) = read_file(dir)
    {
        return channel;
    }
    Channel::from_build()
}

pub(crate) fn read_file(bin_dir: &Path) -> Option<Channel> {
    let text = fs::read_to_string(bin_dir.join(CHANNEL_FILE)).ok()?;
    Channel::parse(text.trim())
}

pub(crate) fn persist(bin_dir: &Path, channel: Channel) -> Result<(), UpdateError> {
    fs::write(
        bin_dir.join(CHANNEL_FILE),
        format!("{}\n", channel.as_str()),
    )
    .map_err(|err| {
        UpdateError::Install(format!(
            "could not record the update channel in {}: {err}",
            bin_dir.join(CHANNEL_FILE).display()
        ))
    })
}

/// Human identity: `0.32.0` on stable, `0.32.0+next.abc1234` on next.
pub(crate) fn display(version: &str, channel: Channel, sha: Option<&str>) -> String {
    match (channel, sha) {
        (Channel::Next, Some(sha)) => {
            let short = if sha.len() >= 7 { &sha[..7] } else { sha };
            format!("{version}+next.{short}")
        }
        (Channel::Next, None) => format!("{version}+next"),
        (Channel::Stable, _) => version.to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::{Channel, display, persist, read_file, resolve};
    use crate::commands::update::source::{Install, InstallSource};

    #[test]
    fn parse_accepts_only_the_closed_vocabulary() {
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse("next"), Some(Channel::Next));
        assert_eq!(Channel::parse("nightly"), None);
        assert_eq!(Channel::parse("latest"), None);
        assert_eq!(Channel::parse(""), None);
    }

    #[test]
    fn persist_round_trips_through_the_bindir_file() {
        let dir = std::env::temp_dir().join(format!(
            "phux-channel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        persist(&dir, Channel::Next).unwrap();
        assert_eq!(read_file(&dir), Some(Channel::Next));
        persist(&dir, Channel::Stable).unwrap();
        assert_eq!(read_file(&dir), Some(Channel::Stable));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_flag_beats_the_bindir_file() {
        let install = Install {
            source: InstallSource::DirectRelease,
            executable: std::path::PathBuf::from("/home/ada/.local/bin/phux"),
            nixos: false,
            unknown_reason: None,
        };
        assert_eq!(resolve(Some(Channel::Next), &install), Channel::Next);
        assert_eq!(resolve(Some(Channel::Stable), &install), Channel::Stable);
    }

    #[test]
    fn display_keeps_stable_as_plain_semver() {
        assert_eq!(
            display("0.32.0", Channel::Stable, Some("deadbeef")),
            "0.32.0"
        );
        assert_eq!(
            display(
                "0.32.0",
                Channel::Next,
                Some("0123456789abcdef0123456789abcdef01234567")
            ),
            "0.32.0+next.0123456"
        );
    }
}
