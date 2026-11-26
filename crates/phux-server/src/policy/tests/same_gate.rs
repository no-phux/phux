/// Rows gated exactly as another one.
fn same_gate_rows(rows: &[Row]) -> Vec<Row> {
    use phux_protocol::open_url::OpenUrlOp;
    let like = |row: &Row, frame| Row {
        frame,
        admit: row.admit.clone(),
        deny: row.deny.clone(),
    };
    // `VIEWPORT_COLORS`: `BIND` on the Terminals the connection views.
    let colors = rows
        .iter()
        .find(|row| matches!(row.frame, FrameKind::ViewportResize { .. }))
        .map(|row| like(row, viewport_colors()));
    // `OPEN_URL` and its reply: `INPUT` on the Terminal, as `TRANSCRIBE`.
    let (row, terminal_id) = rows
        .iter()
        .find_map(|row| match &row.frame {
            FrameKind::Command {
                command: Command::Transcribe { terminal_id, .. },
                ..
            } => Some((row, terminal_id)),
            _ => None,
        })
        .unwrap();
    let url = "https://phux.sh/".to_owned();
    let ops = [
        OpenUrlOp::Request { url },
        OpenUrlOp::Reply {
            id: 1,
            opened: true,
        },
    ];
    let open_url = ops.map(|op| {
        let command = Command::OpenUrl {
            terminal_id: terminal_id.clone(),
            op,
        };
        like(
            row,
            FrameKind::Command {
                request_id: 1,
                command,
            },
        )
    });
    colors.into_iter().chain(open_url).collect()
}
