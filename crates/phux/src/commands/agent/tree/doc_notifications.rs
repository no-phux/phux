//! `phux agent doc notifications`: this host's desktop notifications as a
//! Markdown document, newest first. On Linux it monitors the session bus
//! for freedesktop `Notify` calls; macOS keeps Notification Center to
//! itself, so there it refuses.

use std::path::PathBuf;
use std::process::ExitCode;

/// One notification as the document shows it.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Notice {
    /// Local wall-clock time it arrived, `HH:MM:SS`.
    at: String,
    app: String,
    summary: String,
    body: String,
}

/// The Markdown document for `notices`, newest first.
#[cfg(any(target_os = "linux", test))]
fn render(notices: &[Notice]) -> String {
    use std::fmt::Write as _;

    let mut doc = String::from("# Notifications\n\n");
    if notices.is_empty() {
        doc.push_str("_None yet._\n");
    }
    for notice in notices {
        let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let app = if notice.app.is_empty() {
            "?"
        } else {
            &notice.app
        };
        let _ = write!(
            doc,
            "- **{}** · {} — **{}**",
            notice.at,
            flat(app),
            flat(&notice.summary)
        );
        let body = flat(&notice.body);
        if !body.is_empty() {
            let _ = write!(doc, ": {body}");
        }
        doc.push('\n');
    }
    doc
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run(
    _name: Option<&str>,
    _pane: Option<&str>,
    _keep: Option<usize>,
    _socket: Option<PathBuf>,
) -> ExitCode {
    eprintln!(
        "phux agent doc notifications: needs Linux (freedesktop Notifications on the \
         session bus); macOS does not let other programs read Notification Center"
    );
    ExitCode::from(crate::exit_codes::EXIT_FAILURE)
}

#[cfg(target_os = "linux")]
pub(super) fn run(
    name: Option<&str>,
    pane: Option<&str>,
    keep: Option<usize>,
    socket: Option<PathBuf>,
) -> ExitCode {
    use std::collections::HashMap;

    use futures_util::StreamExt as _;
    use zbus::{Connection, MessageStream, OwnedVariant};

    use super::doc_producer::Producer;
    use super::report;

    type Notify = (
        String,
        u32,
        String,
        String,
        String,
        Vec<String>,
        HashMap<String, OwnedVariant>,
        i32,
    );
    let keep = keep.unwrap_or(50).max(1);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let bus = async {
            let bus = Connection::session().await?;
            let rule =
                "type='method_call',interface='org.freedesktop.Notifications',member='Notify'";
            bus.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus.Monitoring"),
                "BecomeMonitor",
                &(vec![rule], 0_u32),
            )
            .await?;
            zbus::Result::Ok(MessageStream::from(bus))
        };
        let mut calls = match bus.await {
            Ok(calls) => calls,
            Err(err) => {
                eprintln!("phux agent doc notifications: cannot monitor the session bus: {err}");
                return ExitCode::from(crate::exit_codes::EXIT_FAILURE);
            }
        };
        let name = name.unwrap_or("notifications");
        let verb = "agent doc notifications";
        let opened = Producer::open(socket, pane, "text/markdown", Some(name), verb).await;
        let mut producer = match opened {
            Ok(producer) => producer,
            Err(code) => return code,
        };
        let mut notices: Vec<Notice> = Vec::new();
        loop {
            if let Err(err) = producer.publish(render(&notices).into_bytes()).await {
                return report(false, &err);
            }
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                    woke = producer.recv() => {
                        if let Err(err) = producer.settle(woke).await {
                            return report(false, &err);
                        }
                    }
                    message = calls.next() => {
                        let Some(Ok(message)) = message else {
                            eprintln!("phux agent doc notifications: the session bus closed");
                            return ExitCode::from(crate::exit_codes::EXIT_FAILURE);
                        };
                        let Ok((app, _, _, summary, body, ..)) =
                            message.body().deserialize::<Notify>()
                        else {
                            continue;
                        };
                        let at = chrono::Local::now().format("%H:%M:%S").to_string();
                        notices.insert(0, Notice { at, app, summary, body });
                        notices.truncate(keep);
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_render_newest_first_on_one_line_each() {
        assert_eq!(render(&[]), "# Notifications\n\n_None yet._\n");
        let notice = |at: &str, app: &str, summary: &str, body: &str| Notice {
            at: at.into(),
            app: app.into(),
            summary: summary.into(),
            body: body.into(),
        };
        let doc = render(&[
            notice(
                "12:01:02",
                "Firefox",
                "Download complete",
                "file.zip\nin  Downloads",
            ),
            notice("12:00:00", "", "Ping", ""),
        ]);
        assert_eq!(
            doc,
            "# Notifications\n\n\
             - **12:01:02** · Firefox — **Download complete**: file.zip in Downloads\n\
             - **12:00:00** · ? — **Ping**\n"
        );
    }
}
