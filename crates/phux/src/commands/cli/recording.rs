//! `phux rec`.

use crate::commands::{JsonOpt, RecFormat, rec::RecAction};
use usage::Args;

/// Record a pane and export it as an asciinema cast, an animated GIF, or
/// an APNG.
#[derive(Debug, Args)]
#[usage(
    about = "Record a pane to a cast, GIF, or APNG",
    long_about = "Record a pane and export it as an asciinema cast, an animated GIF, or an APNG.\n\n\
            TARGET is a selector (default: the focused pane). Recording one pane is a pure \
            observer: it does not attach the session and never resizes the pane, so it is safe \
            to run against a live session someone is using.\n\n\
            --composited records the whole session the way `phux --rec` does (tiled panes, \
            dividers, status bar, sidebar) into a virtual viewport. It attaches with a zero \
            size vote, so real panes keep their size. --size is pixels of the recording face \
            (Spleen 8x16); 1920x1080 is 240x67 cells and is the default.\n\n\
            The format follows the output extension (.cast, .gif, .png, .apng); pass --format \
            to override. Use --from to re-render an existing recording without capturing \
            anything. --speed-marked and --marker-labels apply when rendering a GIF or APNG; \
            the .cast keeps original timing and asciicast `m` markers.\n\n\
            `phux rec mark LABEL` posts a marker to every recording in progress (the same \
            event as the `rec-mark` action during `phux --rec`).\n\n\
            Examples:\n  \
            phux rec -o demo.gif\n  \
            phux rec work:1.0 -o demo.cast --duration 30\n  \
            phux rec --composited work --size 1920x1080 -o demo.cast\n  \
            phux rec --from demo.cast -o demo.gif --speed-marked 2 --marker-labels\n  \
            phux rec mark \"label\""
)]
pub(crate) struct RecArgs {
    #[usage(subcommand)]
    pub(crate) action: Option<RecAction>,

    /// Pane selector, or the session to composite. Defaults to the
    /// focused pane, or the attached session with --composited.
    #[usage(value_name = "TARGET")]
    pub(crate) target: Option<String>,

    /// Output path. The extension picks the format unless --format is
    /// given; a path with no extension gets `.gif`. Required to capture
    /// or re-render; `phux rec mark` does not take it.
    #[usage(short = 'o', long = "out", value_name = "PATH")]
    pub(crate) out: Option<std::path::PathBuf>,

    /// Output format, overriding the extension.
    #[usage(long, value_enum, value_name = "FMT")]
    pub(crate) format: Option<RecFormat>,

    /// Re-render an existing .cast instead of capturing a live pane.
    #[usage(
        long,
        value_name = "FILE",
        conflicts("--target", "--duration", "--composited")
    )]
    pub(crate) from: Option<std::path::PathBuf>,

    /// Stop after SECS of recording (default: until Ctrl-C or the pane
    /// exits).
    #[usage(long, value_name = "SECS")]
    pub(crate) duration: Option<u64>,

    /// Animation sample rate for GIF/APNG output.
    #[usage(
        long,
        value_name = "FPS",
        default = "10",
        default_value_t = 10,
        validate = "int(value) >= 1 && int(value) <= 50",
        validate_error = "must be between 1 and 50"
    )]
    pub(crate) fps: u8,

    /// Collapse any pause longer than SECS down to SECS. 0 disables.
    #[usage(
        long = "idle-limit",
        value_name = "SECS",
        default = "2.0",
        default_value_t = 2.0
    )]
    pub(crate) idle_limit: f64,

    /// Stop encoding and warn once the output reaches BYTES.
    #[usage(long = "max-bytes", value_name = "BYTES", default = "8388608", default_value_t = 8 * 1024 * 1024)]
    pub(crate) max_bytes: u64,

    /// asciicast format version to write (2 is the interoperable
    /// default).
    #[usage(
        long = "cast-version",
        value_name = "N",
        default = "2",
        default_value_t = 2,
        validate = "int(value) >= 2 && int(value) <= 3",
        validate_error = "must be 2 or 3"
    )]
    pub(crate) cast_version: u8,

    /// Record the session composite at a virtual viewport. Attaches with
    /// a zero size vote and never resizes real panes.
    #[usage(long, conflicts("--from"))]
    pub(crate) composited: bool,

    /// Pixel size of the virtual viewport, `WIDTHxHEIGHT`. `1920x1080` is
    /// the default. The cast grid is that size floored to an 8x16 cell.
    #[usage(long, value_name = "WIDTHxHEIGHT", requires("--composited"))]
    pub(crate) size: Option<String>,

    /// Play each marked span this many times faster when rendering a GIF
    /// or APNG. Greater than 1 and at most 100. The .cast is unchanged.
    #[usage(long = "speed-marked", value_name = "FACTOR")]
    pub(crate) speed_marked: Option<f64>,

    /// Draw the opening marker's label on rendered GIF/APNG frames.
    #[usage(long = "marker-labels")]
    pub(crate) marker_labels: bool,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}
