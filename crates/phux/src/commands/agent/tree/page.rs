//! `phux agent page mirror`: a browser page as a tree and a surface.

use std::path::PathBuf;
use std::process::ExitCode;

use super::page_mirror;

/// `phux agent page mirror`: drive a browser page over the Chrome debugging protocol (CDP).
#[derive(Debug, usage::Subcommands)]
pub(crate) enum PageAction {
    /// Mirror a browser page into a tree and a surface until Ctrl-C.
    Mirror {
        /// A fragment of the page's title or URL, or `first`.
        page: String,
        /// The browser's debugging port (default 9222).
        #[usage(long, value_name = "PORT")]
        port: Option<u16>,
        /// The pane the documents bind to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between accessibility-tree reads (default 1000).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        #[usage(long)]
        json: bool,
    },
}

pub(crate) fn run_page(action: &PageAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        PageAction::Mirror {
            page,
            port,
            pane,
            interval,
            json,
        } => page_mirror::run_mirror(page_mirror::MirrorArgs {
            page,
            port: port.unwrap_or(9222),
            pane: pane.as_deref(),
            interval_ms: interval.unwrap_or(1000),
            json: *json,
            socket,
        }),
    }
}
