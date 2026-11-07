//! `phux agent surface open|publish|snapshot|follow|mirror`: the verbs over
//! a `PixelSurface`, whose documents are PNG images and go to files.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::Kind;

use super::{Sink, run_follow, run_open, run_publish, run_snapshot, surface_mirror};

/// `phux agent surface open|publish|snapshot|follow`: the same verbs over a
/// `PixelSurface`, whose documents are PNG images and go to files.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum SurfaceAction {
    /// Open a surface under the pane its producer runs in.
    Open {
        /// The parent pane.
        target: String,
        /// The window or display the surface mirrors.
        #[usage(long, value_name = "WINDOW")]
        window: String,
        #[usage(long)]
        json: bool,
    },
    /// Replace the surface's image from a PNG file, or stdin.
    Publish {
        /// The surface.
        target: String,
        /// The PNG; stdin when absent.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Write the surface's newest image to a file.
    Snapshot {
        /// The surface.
        target: String,
        /// Where the PNG goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Mirror a macOS window, or the main display, into a surface until Ctrl-C.
    Mirror {
        /// The window as `App` or `App:title`, or `display` for the main display.
        window: String,
        /// The CoreGraphics window id, instead of a name.
        #[usage(long, value_name = "ID")]
        window_id: Option<u32>,
        /// The pane the surface binds to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between captures (default 1000).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        /// Exit once this process is gone, as the app that launched the mirror passes its own.
        #[usage(long, value_name = "PID")]
        parent_pid: Option<u32>,
        #[usage(long)]
        json: bool,
    },
    /// Write every image as it arrives, each replacing the file.
    Follow {
        /// The surface.
        target: String,
        /// Where each PNG goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Give up after this many seconds.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        #[usage(long)]
        json: bool,
    },
}

pub(crate) fn run_surface(action: &SurfaceAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        SurfaceAction::Open {
            target,
            window,
            json,
        } => run_open(Kind::Surface, target, window, *json, socket),
        SurfaceAction::Publish { target, file, json } => {
            run_publish(Kind::Surface, target, file.as_deref(), *json, socket)
        }
        SurfaceAction::Snapshot { target, out, json } => {
            run_snapshot(Kind::Surface, target, out.as_deref(), *json, socket)
        }
        SurfaceAction::Mirror {
            window,
            window_id,
            pane,
            interval,
            parent_pid,
            json,
        } => {
            super::parent_watch::exit_with(*parent_pid);
            surface_mirror::run_mirror(surface_mirror::MirrorArgs {
                window,
                window_id: *window_id,
                pane: pane.as_deref(),
                interval_ms: interval.unwrap_or(1000),
                json: *json,
                socket,
            })
        }
        SurfaceAction::Follow {
            target,
            out,
            timeout,
            json,
        } => run_follow(
            Kind::Surface,
            target,
            Sink {
                until: None,
                out: out.as_deref(),
            },
            *timeout,
            *json,
            socket,
        ),
    }
}
