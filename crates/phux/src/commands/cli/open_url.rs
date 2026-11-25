//! `phux open-url`.

use usage::Args;

/// Ask to open a link on your own machine.
#[derive(Debug, Args)]
#[usage(
    about = "Ask to open a link on your own machine",
    long_about = "Open a link on the machine of the person typing in this pane.\n\n\
            Run inside a phux pane. The attached client that last typed here asks \
            its user, showing the whole link; only http and https links are asked \
            about. Exits 0 once the link is opened, 1 when it was not: declined, \
            unanswered for 60 seconds, or no attached client can ask.\n\n\
            `[defaults] browser-shim` makes panes' programs use it.\n\n\
            Example:\n  \
            phux open-url https://phux.sh"
)]
pub(crate) struct OpenUrlArgs {
    /// The link.
    pub(crate) url: String,
}
