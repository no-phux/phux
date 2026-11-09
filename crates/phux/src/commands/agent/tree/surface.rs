//! `phux agent surface open|publish|snapshot|follow|mirror`: the verbs over
//! a `PixelSurface`, whose documents are PNG or JPEG images and go to
//! files.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::Kind;
use usage::ValueEnum;

use crate::commands::json_err;
use crate::exit_codes::EXIT_USAGE;

use super::{Sink, run_follow, run_open, run_publish, run_snapshot, surface_mirror};

/// `phux agent surface open|publish|snapshot|follow`: the same verbs over a
/// `PixelSurface`, whose documents are PNG or JPEG images and go to files.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum SurfaceAction {
    /// Open a surface under the pane its producer runs in.
    Open {
        /// The parent pane.
        target: String,
        /// The window or display the surface mirrors.
        #[usage(long, value_name = "WINDOW")]
        window: String,
        /// What input to the surface controls: `page`, `window`, or
        /// `display`. Only a device granted that control class can send it
        /// input; without this flag only the owner socket can.
        #[usage(long, value_enum, value_name = "CLASS")]
        control: Option<Control>,
        #[usage(long)]
        json: bool,
    },
    /// Replace the surface's image from a PNG or JPEG file, or stdin.
    Publish {
        /// The surface.
        target: String,
        /// The PNG or JPEG; stdin when absent.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Write the surface's newest image to a file.
    Snapshot {
        /// The surface.
        target: String,
        /// Where the image goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Mirror a window or a display into a surface until Ctrl-C: macOS, or
    /// Linux on a wlroots compositor (Hyprland, Sway) through `grim`.
    Mirror {
        /// The window as `App` or `App:title` (on Linux, a Hyprland class), or
        /// `display`: the main display on macOS, every output on Linux, where
        /// `display:OUTPUT` names one.
        window: String,
        /// The CoreGraphics window id, instead of a name (macOS).
        #[usage(long, value_name = "ID")]
        window_id: Option<u32>,
        /// The pane the surface binds to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between captures while no input arrives (default
        /// 1000). After input it captures 50ms later, then every 150ms for 2s.
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        /// Exit once this process is gone, as the app that launched the mirror passes its own.
        #[usage(long, value_name = "PID")]
        parent_pid: Option<u32>,
        #[usage(flatten)]
        frames: FrameOpts,
        /// Log the input events viewers' intents would post (macOS), or the
        /// Wayland requests and commands they would send (Linux), instead;
        /// a refused capture publishes a blank frame.
        #[usage(long)]
        dry_run_input: bool,
        #[usage(long)]
        json: bool,
    },
    /// Run the `[[mirrors]]` entries as this process's children, each into
    /// its `mirror-<target>` session, restarting any that exit. Rune runs
    /// this on macOS, where the server's own children cannot hold Screen
    /// Recording access; one runs per server.
    Standing {
        /// Exit once this process is gone, as the app that launched it passes its own.
        #[usage(long, value_name = "PID")]
        parent_pid: Option<u32>,
    },
    /// Write every image as it arrives, each replacing the file.
    Follow {
        /// The surface.
        target: String,
        /// Where each image goes; stdout when absent.
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
            control,
            json,
        } => {
            let control = control.map(Control::class);
            run_open(Kind::Surface, target, window, control, *json, socket)
        }
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
            frames,
            dry_run_input,
            json,
        } => {
            super::parent_watch::exit_with(*parent_pid);
            match frames.resolve(*json, FrameFormat::Png) {
                Ok(frames) => surface_mirror::run_mirror(surface_mirror::MirrorArgs {
                    window,
                    window_id: *window_id,
                    pane: pane.as_deref(),
                    interval_ms: interval.unwrap_or(1000),
                    frames,
                    dry_run_input: *dry_run_input,
                    json: *json,
                    socket,
                }),
                Err(code) => code,
            }
        }
        SurfaceAction::Standing { parent_pid } => {
            super::parent_watch::exit_with(*parent_pid);
            super::surface_standing::run_standing(socket)
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

/// How a mirror encodes its frames: `--format`, `--quality`, `--max-width`.
#[derive(Debug, usage::Args)]
pub(crate) struct FrameOpts {
    /// Frame encoding: jpeg by default for a page mirror, png for a window
    /// or display mirror. JPEG frames are several times smaller; viewers
    /// older than JPEG support (desktop before 0.57.0, Rune before
    /// 0.1.0-alpha.17) show them as invalid.
    #[usage(long, value_enum, value_name = "FORMAT")]
    format: Option<FrameFormat>,
    /// JPEG quality, 1 to 100 (default 70).
    #[usage(long, value_name = "Q")]
    quality: Option<u8>,
    /// Scale frames down to at most this many pixels wide.
    #[usage(long, value_name = "PX")]
    max_width: Option<u32>,
}

/// A surface frame's encoding.
/// What input to a surface controls (`docs/resource-kinds.md#authority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Control {
    /// One browser page.
    Page,
    /// One application window.
    Window,
    /// A whole screen: every application on it.
    Display,
}

impl Control {
    const fn class(self) -> phux_protocol::wire::frame::ControlClass {
        use phux_protocol::wire::frame::ControlClass;
        match self {
            Self::Page => ControlClass::Page,
            Self::Window => ControlClass::Window,
            Self::Display => ControlClass::Display,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum FrameFormat {
    /// Lossless PNG.
    Png,
    /// JPEG at `--quality`.
    Jpeg,
}

/// [`FrameOpts`], checked: the JPEG quality when frames are JPEG, and the
/// widest frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Frames {
    pub(super) jpeg: Option<u8>,
    pub(super) max_width: Option<u32>,
}

impl FrameOpts {
    /// `default` is the format when `--format` is not given.
    pub(super) fn resolve(&self, json: bool, default: FrameFormat) -> Result<Frames, ExitCode> {
        let usage = |message: &str| {
            json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::USAGE_ERROR,
                    message,
                    "e.g. `--format jpeg --quality 70 --max-width 1280`",
                ),
                EXIT_USAGE,
            )
        };
        let quality = self.quality.unwrap_or(70);
        if !(1..=100).contains(&quality) {
            return Err(usage("`--quality` is 1 to 100"));
        }
        if self.max_width == Some(0) {
            return Err(usage("`--max-width` is at least 1"));
        }
        let jpeg = self.format.unwrap_or(default) == FrameFormat::Jpeg;
        if self.quality.is_some() && !jpeg {
            return Err(usage("`--quality` needs `--format jpeg`"));
        }
        Ok(Frames {
            jpeg: jpeg.then_some(quality),
            max_width: self.max_width,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_options_default_per_mirror_and_check_quality_and_width() {
        let opts = |format, quality, max_width| FrameOpts {
            format,
            quality,
            max_width,
        };
        let png = Frames {
            jpeg: None,
            max_width: None,
        };
        let png_default = |o: FrameOpts| o.resolve(true, FrameFormat::Png);
        assert_eq!(png_default(opts(None, None, None)), Ok(png));
        let page = opts(None, None, None).resolve(true, FrameFormat::Jpeg);
        assert_eq!(
            page,
            Ok(Frames {
                jpeg: Some(70),
                max_width: None
            })
        );
        let page_png = opts(Some(FrameFormat::Png), None, None).resolve(true, FrameFormat::Jpeg);
        assert_eq!(page_png, Ok(png));
        let page_q = opts(None, Some(50), None).resolve(true, FrameFormat::Jpeg);
        assert_eq!(
            page_q,
            Ok(Frames {
                jpeg: Some(50),
                max_width: None
            })
        );
        let jpeg = png_default(opts(Some(FrameFormat::Jpeg), None, Some(800)));
        assert_eq!(
            jpeg,
            Ok(Frames {
                jpeg: Some(70),
                max_width: Some(800),
            })
        );
        for bad in [
            opts(Some(FrameFormat::Jpeg), Some(0), None),
            opts(Some(FrameFormat::Jpeg), Some(101), None),
            opts(Some(FrameFormat::Png), Some(70), None),
            opts(None, Some(70), None),
            opts(None, None, Some(0)),
        ] {
            assert!(png_default(bad).is_err());
        }
    }
}
