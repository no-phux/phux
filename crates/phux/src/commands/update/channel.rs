//! Which release channel an install follows, and how it is remembered.

use std::fs;
use std::path::Path;

use phux_client_runtime::release::{Build, Channel, Version};

use super::UpdateError;
use super::source::Install;

/// Persisted beside the binaries so the next `phux update` stays on the
/// channel the user opted into, even when `--channel` is omitted.
pub(crate) const CHANNEL_FILE: &str = ".phux-channel";

/// The `--channel` flag. Its own type only because the derive must live in
/// this crate; it converts straight to [`Channel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, usage::ValueEnum)]
pub(crate) enum ChannelArg {
    /// `vX.Y.Z` releases, named by `channels/stable.json`.
    #[usage(alias = "latest")]
    Stable,
    /// The moving prerelease of green `main`.
    Next,
}

impl From<ChannelArg> for Channel {
    fn from(arg: ChannelArg) -> Self {
        match arg {
            ChannelArg::Stable => Self::Stable,
            ChannelArg::Next => Self::Next,
        }
    }
}

/// The channel this binary was compiled as.
pub(crate) fn of_build() -> Channel {
    match option_env!("PHUX_BUILD_CHANNEL") {
        Some("next") => Channel::Next,
        _ => Channel::Stable,
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
/// channel, so a next binary stays on next after its first install.
pub(crate) fn resolve(explicit: Option<Channel>, install: &Install) -> Channel {
    explicit
        .or_else(|| install.bin_dir().and_then(read_file))
        .unwrap_or_else(of_build)
}

/// The build `install` holds now, on the channel it last installed from.
pub(crate) fn installed(install: &Install, version: Version) -> Build {
    match install
        .bin_dir()
        .and_then(read_file)
        .unwrap_or_else(of_build)
    {
        Channel::Stable => Build::Stable(version),
        Channel::Next => Build::Next(build_sha().unwrap_or_default().to_owned()),
    }
}

pub(crate) fn read_file(bin_dir: &Path) -> Option<Channel> {
    let text = fs::read_to_string(bin_dir.join(CHANNEL_FILE)).ok()?;
    Channel::parse(text.trim())
}

pub(crate) fn persist(bin_dir: &Path, channel: Channel) -> Result<(), UpdateError> {
    let path = bin_dir.join(CHANNEL_FILE);
    fs::write(&path, format!("{}\n", channel.as_str())).map_err(|err| {
        UpdateError::Install(format!(
            "could not record the update channel in {}: {err}",
            path.display()
        ))
    })
}

/// Human identity: `0.32.0` on stable, `0.32.0+next.abc1234` on next.
pub(crate) fn display(version: &str, channel: Channel, sha: Option<&str>) -> String {
    match (channel, sha) {
        (Channel::Next, Some(sha)) => format!("{version}+next.{}", &sha[..sha.len().min(7)]),
        (Channel::Next, None) => format!("{version}+next"),
        (Channel::Stable, _) => version.to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_client_runtime::release::Channel;

    use super::{display, persist, read_file, resolve};
    use crate::commands::update::source::{Install, InstallSource};

    #[test]
    fn the_flag_beats_the_file_and_the_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let install = Install {
            source: InstallSource::DirectRelease,
            executable: dir.path().join("phux"),
            nixos: false,
            unknown_reason: None,
        };
        persist(dir.path(), Channel::Next).unwrap();
        assert_eq!(read_file(dir.path()), Some(Channel::Next));
        assert_eq!(resolve(None, &install), Channel::Next);
        assert_eq!(resolve(Some(Channel::Stable), &install), Channel::Stable);
    }

    #[test]
    fn next_builds_display_their_short_sha() {
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
