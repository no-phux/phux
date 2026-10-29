/// `phux_ls` — the `phux ls --json` document. Never auto-starts a server; a
/// partial listing names what could not be seen in `unreachable`.
async fn phux_ls(args: &Value) -> Result<Value, ToolError> {
    strict_object(args, &["socket"], &[])?;
    let socket = socket_arg(args)?;
    let mut conn = Connection::connect(&socket).await?;
    let view = phux_client::session_list::listing_view(&mut conn).await?;
    drop(conn);
    let hosts_complete = view.host_sessions_complete();
    let organization = view.organization().cloned();
    let (snapshot, degradation) = view.into_parts();
    let list = phux_client::session_list::document(
        &snapshot,
        organization.as_ref(),
        &degradation,
        hosts_complete,
    );
    serde_json::to_value(&list)
        .map_err(|err| ToolError::new(format!("failed to serialize session list: {err}")))
}

/// `phux_snapshot` — read a pane as structured data. `scrollback` is
/// tri-state (absent: viewport; `0`: all history; `N`: last `N` rows);
/// `tail`/`unwrap` are the ADR-0077 projections `phux snapshot` applies too.
async fn phux_snapshot(args: &Value) -> Result<Value, ToolError> {
    let socket = socket_or_default(args);
    let selector = parse_target(args)?;
    let read = SnapshotRead::parse(args)?;
    let terminal_id = resolve_selector(&socket, &selector, false).await?;
    let screen = phux_client::snapshot::get_screen_scrollback_format(
        &socket,
        terminal_id,
        read.history(),
        read.cells,
        read.format_byte(),
    )
    .await?;
    read.document(screen)
}

/// One validated `phux_snapshot` request.
struct SnapshotRead<'a> {
    scrollback: Option<u32>,
    cells: bool,
    tail: Option<u32>,
    unwrap: bool,
    format: Option<&'a str>,
}

impl<'a> SnapshotRead<'a> {
    fn parse(args: &'a Value) -> Result<Self, ToolError> {
        let read = Self {
            scrollback: u32_arg(args, "scrollback"),
            cells: bool_arg(args, "cells").unwrap_or(false),
            tail: u32_arg(args, "tail"),
            unwrap: bool_arg(args, "unwrap").unwrap_or(false),
            format: snapshot_format_arg(args)?,
        };
        if read.unwrap && read.cells {
            return Err(ToolError::new(
                "`unwrap` cannot be combined with `cells`: cell coordinates are grid \
                 coordinates and do not survive the join",
            ));
        }
        if read.format.is_some() && read.cells {
            return Err(ToolError::new(
                "`format` cannot be combined with `cells`: a rendered capture carries \
                 no per-cell projection",
            ));
        }
        Ok(read)
    }

    /// The history window to request: `tail` asks for history too.
    const fn history(&self) -> Option<u32> {
        phux_client::snapshot::history_request(self.scrollback, self.tail)
    }

    /// `GET_SCREEN`'s format byte; with `format`, `unwrap` asks the server's
    /// Formatter to join soft-wrapped rows (its high bit).
    fn format_byte(&self) -> u8 {
        let unwrap = if self.unwrap {
            phux_client::snapshot::SCREEN_FORMAT_UNWRAP
        } else {
            0
        };
        self.format
            .map_or(0, |format| format_wire_byte(format) | unwrap)
    }

    /// The document, projected by the modifiers and bounded when one ran.
    fn document(&self, screen: ScreenState) -> Result<Value, ToolError> {
        let screen = phux_client::snapshot::project(screen, self.unwrap, self.tail);
        if self.tail.is_none() && !self.unwrap {
            return serde_json::to_value(&screen)
                .map_err(|err| ToolError::new(format!("failed to serialize screen: {err}")));
        }
        phux_client::snapshot::bounded_document(&screen, PROJECTED_DOCUMENT_MAX_BYTES)
            .map_err(|err| ToolError::new(err.to_string()))
    }
}

/// The optional `format` argument: `"html"` or `"vt"`.
fn snapshot_format_arg(args: &Value) -> Result<Option<&str>, ToolError> {
    match str_arg(args, "format") {
        None => Ok(None),
        Some(value @ ("html" | "vt")) => Ok(Some(value)),
        Some(other) => Err(ToolError::new(format!(
            "unknown `format`: {other:?} (expected \"html\" or \"vt\")"
        ))),
    }
}

/// `GET_SCREEN`'s wire byte for a validated `format` string.
fn format_wire_byte(format: &str) -> u8 {
    if format == "html" {
        phux_client::snapshot::SCREEN_FORMAT_HTML
    } else {
        phux_client::snapshot::SCREEN_FORMAT_VT
    }
}
