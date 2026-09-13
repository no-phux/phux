//! `phux channel` — show or switch the release rail.
//!
//! Bare `phux channel` is `phux update --check` with the channel on the first
//! line of the human view. `phux channel next` / `phux channel latest` persist
//! that rail and perform the update. The trust path is the updater's.

use std::path::PathBuf;
use std::process::ExitCode;

use super::update::channel::Channel;
use super::update::{UpdateOpts, run_update};

/// Show the current rail, or switch to `channel` and install it.
pub(crate) fn run(channel: Option<Channel>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_update(
        &UpdateOpts {
            check: channel.is_none(),
            dry_run: false,
            tag: None,
            channel,
            rollback: false,
            no_restart: false,
            json,
        },
        socket,
    )
}

#[cfg(test)]
mod tests {
    use super::Channel;

    #[test]
    fn latest_is_stable() {
        assert_eq!(Channel::parse("latest"), Some(Channel::Stable));
        assert_eq!(Channel::parse("stable"), Some(Channel::Stable));
        assert_eq!(Channel::parse("next"), Some(Channel::Next));
    }
}
