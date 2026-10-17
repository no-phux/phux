//! `phux channel` — show or switch the release channel.
//!
//! Bare `phux channel` is `phux update --check` with the channel on the first
//! line of the human view. `phux channel next` / `phux channel latest` persist
//! that channel and perform the update. The trust path is the updater's.

use std::path::PathBuf;
use std::process::ExitCode;

use super::update::channel::ChannelArg;
use super::update::{UpdateOpts, run_update};

/// Show the current channel, or switch to `channel` and install it.
pub(crate) fn run(channel: Option<ChannelArg>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_update(
        &UpdateOpts {
            check: channel.is_none(),
            dry_run: false,
            tag: None,
            channel,
            rollback: false,
            no_restart: false,
            close_agent_sessions: false,
            json,
        },
        socket,
    )
}
