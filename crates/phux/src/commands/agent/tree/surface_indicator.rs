//! The control indicator: while a viewer's input has reached the desktop in
//! the last [`IDLE`], the host says so, so nobody is driven unawares.
//!
//! - Linux: one desktop notification over the freedesktop `Notifications`
//!   D-Bus API ("phux: iPhone is controlling this display"), replaced when
//!   the controller changes and closed once input stops.
//! - macOS: one Notification Center banner per burst of control
//!   (`osascript`); a banner cannot be withdrawn, so it simply fades.
//!
//! The name is the viewer's HELLO name, which the server sends ahead of each
//! intent as an `input_activity`. A dry run logs instead of notifying.

#![cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(dead_code, reason = "only the macOS and Linux producers show it")
)]

use std::time::{Duration, Instant};

/// How long after the last input the indicator stays up.
pub(super) const IDLE: Duration = Duration::from_secs(10);
/// Who controls when no `input_activity` named them.
pub(super) const UNNAMED: &str = "a viewer";

/// Whether someone is in control, and who is shown.
#[derive(Debug, Default)]
struct Control {
    last: Option<Instant>,
    shown: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    Show(String),
    Hide,
}

impl Control {
    /// Input from `origin` arrived at `now`.
    fn input(&mut self, origin: &str, now: Instant) -> Option<Change> {
        self.last = Some(now);
        (self.shown.as_deref() != Some(origin)).then(|| {
            self.shown = Some(origin.to_owned());
            Change::Show(origin.to_owned())
        })
    }

    /// The indicator's change at `now`, once input has stopped.
    fn tick(&mut self, now: Instant) -> Option<Change> {
        let idle = self
            .last
            .is_some_and(|last| now.duration_since(last) >= IDLE);
        (idle && self.shown.take().is_some()).then_some(Change::Hide)
    }
}

/// The control indicator of one mirror.
pub(super) struct Indicator {
    control: Control,
    /// `display` or `window`.
    what: &'static str,
    dry_run: bool,
    #[cfg(target_os = "linux")]
    notice: linux::Notice,
}

impl Indicator {
    pub(super) fn new(display: bool, dry_run: bool) -> Self {
        Self {
            control: Control::default(),
            what: if display { "display" } else { "window" },
            dry_run,
            #[cfg(target_os = "linux")]
            notice: linux::Notice::default(),
        }
    }

    /// `origin`'s input was just carried out.
    pub(super) async fn input(&mut self, origin: &str) {
        if let Some(change) = self.control.input(origin, Instant::now()) {
            self.apply(change).await;
        }
    }

    /// Take the indicator down if input has stopped.
    pub(super) async fn tick(&mut self) {
        if let Some(change) = self.control.tick(Instant::now()) {
            self.apply(change).await;
        }
    }

    #[cfg_attr(
        not(target_os = "linux"),
        allow(
            clippy::unused_async,
            clippy::unused_async_trait_impl,
            clippy::needless_pass_by_ref_mut,
            reason = "only the Linux notice awaits and keeps state"
        )
    )]
    async fn apply(&mut self, change: Change) {
        let text = |origin: &str| format!("phux: {origin} is controlling this {}", self.what);
        if self.dry_run {
            match &change {
                Change::Show(origin) => eprintln!("dry-run: would notify: {}", text(origin)),
                Change::Hide => eprintln!("dry-run: would close the control notice"),
            }
            return;
        }
        #[cfg(target_os = "linux")]
        if let Err(err) = self
            .notice
            .apply(match change {
                Change::Show(origin) => Some(text(&origin)),
                Change::Hide => None,
            })
            .await
        {
            tracing::debug!(%err, "the control notice was not shown");
        }
        #[cfg(target_os = "macos")]
        if let Change::Show(origin) = change {
            // The text rides argv, so a name cannot break out of the script.
            let mut banner = std::process::Command::new("osascript");
            banner.args([
                "-e",
                "on run argv",
                "-e",
                "display notification (item 1 of argv) with title \"phux\"",
                "-e",
                "end run",
                &text(&origin),
            ]);
            std::thread::spawn(move || banner.output());
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;

    use zbus::{Connection, Variant};

    const NAME: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";

    /// The session bus, and the id of the notification up now (0: none).
    #[derive(Default)]
    pub(super) struct Notice {
        bus: Option<Connection>,
        id: u32,
    }

    impl Notice {
        /// Show `text`, or with `None` close what is shown.
        pub(super) async fn apply(&mut self, text: Option<String>) -> zbus::Result<()> {
            let bus = match &self.bus {
                Some(bus) => bus.clone(),
                None => self.bus.insert(Connection::session().await?).clone(),
            };
            let Some(text) = text else {
                let id = std::mem::take(&mut self.id);
                if id != 0 {
                    bus.call_method(Some(NAME), PATH, Some(NAME), "CloseNotification", &(id,))
                        .await?;
                }
                return Ok(());
            };
            // The text goes in the summary, which servers never read as
            // markup. Low urgency, and no timeout: it is closed.
            let hints = HashMap::from([("urgency", Variant::U8(0))]);
            let body = (
                "phux",
                self.id,
                "",
                text.as_str(),
                "",
                Vec::<&str>::new(),
                hints,
                0_i32,
            );
            let reply = bus
                .call_method(Some(NAME), PATH, Some(NAME), "Notify", &body)
                .await?;
            self.id = reply.body().deserialize::<u32>()?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_once_per_controller_and_hidden_after_idle() {
        let mut control = Control::default();
        let t0 = Instant::now();
        let at = |s| t0 + Duration::from_secs(s);
        assert_eq!(control.tick(at(0)), None);
        assert_eq!(
            control.input("iPhone", at(0)),
            Some(Change::Show("iPhone".into()))
        );
        assert_eq!(control.input("iPhone", at(5)), None);
        assert_eq!(control.tick(at(14)), None);
        // Another viewer takes over: the notice is replaced.
        assert_eq!(
            control.input("mini", at(14)),
            Some(Change::Show("mini".into()))
        );
        assert_eq!(control.tick(at(24)), Some(Change::Hide));
        assert_eq!(control.tick(at(30)), None);
        // The next input shows it again.
        assert_eq!(
            control.input("mini", at(31)),
            Some(Change::Show("mini".into()))
        );
    }
}
