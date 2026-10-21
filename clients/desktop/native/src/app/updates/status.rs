//! What the Updates page and the titlebar say: each product's state, the
//! headline for the one update button, and the notices under it.

use super::*;

impl Updater {
    /// The incoming protocol change this update will not resolve: phux is not
    /// managed here, has nothing to install, or lands on another protocol.
    pub(super) fn unresolved_protocol(&self) -> Option<(String, String)> {
        let (now, incoming) = self.protocol_change()?;
        let lands = self.pending_handoff
            || self.runtime.as_ref().is_some_and(|runtime| {
                runtime
                    .protocol_target
                    .as_deref()
                    .is_none_or(|target| same_protocol_line(target, &incoming))
            });
        (!(lands && self.phux_due())).then_some((now, incoming))
    }

    /// A product's state while the updater works: its own step, or what
    /// waits on another product's step. `None` leaves its resting state.
    pub(super) fn working(&self, desktop: bool) -> Option<(&'static str, Tone)> {
        if !self.busy {
            return None;
        }
        match (self.work, desktop) {
            (Work::Checking, true) => Some(("Checking…", Tone::Quiet)),
            (Work::Checking, false) => self
                .prefs
                .manage_runtime
                .then_some(("Checking…", Tone::Quiet)),
            (Work::Downloading, true) => Some(("Downloading…", Tone::Ready)),
            (Work::Downloading, false) => {
                (self.chained && self.phux_due()).then_some(("Up next", Tone::Quiet))
            }
            (Work::Installing, true) => self
                .staged
                .is_some()
                .then_some(("Restarts next", Tone::Quiet)),
            (Work::Installing, false) => Some(("Installing…", Tone::Ready)),
        }
    }

    pub(super) fn desktop_status(&self) -> (String, Tone) {
        let (status, tone) = if let Some(working) = self.working(true) {
            working
        } else if self.development {
            ("Development", Tone::Quiet)
        } else if failure(&self.message, "Desktop:").is_some() {
            ("Failed", Tone::Bad)
        } else if self.staged.is_some() {
            ("Downloaded", Tone::Ready)
        } else if self.desktop_available() {
            ("Update ready", Tone::Ready)
        } else if self.target.is_some() {
            ("Up to date", Tone::Good)
        } else {
            ("Not checked", Tone::Quiet)
        };
        (status.to_owned(), tone)
    }

    /// Desktop's installed version, and the one this update brings in.
    pub(super) fn desktop_versions(&self) -> (String, Option<String>) {
        let installed = self.current.as_ref().map_or_else(
            || "development".to_owned(),
            |release| release.version.clone(),
        );
        (
            installed,
            self.incoming().map(|release| release.version.clone()),
        )
    }

    /// phux's installed version, and the one an update would install.
    pub(super) fn runtime_versions(&self) -> (String, Option<String>) {
        let Some(runtime) = self.runtime.as_ref().filter(|_| self.prefs.manage_runtime) else {
            return ("—".to_owned(), None);
        };
        let incoming = runtime.available.then(|| runtime.target.clone());
        (runtime.current.clone(), incoming)
    }

    pub(super) fn runtime_status(&self) -> (String, Tone) {
        let (status, tone) = if let Some(working) = self.working(false) {
            working
        } else if !self.prefs.manage_runtime {
            ("Not managed", Tone::Quiet)
        } else if failure(&self.message, "phux:").is_some() {
            ("Failed", Tone::Bad)
        } else if self.pending_handoff {
            ("Handoff pending", Tone::Ready)
        } else if self.runtime_available() {
            ("Update ready", Tone::Ready)
        } else if self
            .runtime
            .as_ref()
            .is_some_and(|runtime| runtime.available)
        {
            ("Package managed", Tone::Quiet)
        } else if self.runtime.is_some() {
            ("Up to date", Tone::Good)
        } else {
            ("Not checked", Tone::Quiet)
        };
        (status.to_owned(), tone)
    }

    /// The page's headline: what the update button would do, or why it is absent.
    pub(super) fn summary(&self) -> (String, Tone) {
        if self.busy {
            return (self.message.clone(), Tone::Ready);
        }
        if self.development {
            return ("Self-update is off for this build".to_owned(), Tone::Quiet);
        }
        let headline = match self.action() {
            _ if self.update_failed() => ("The update did not finish", Tone::Bad),
            _ if self.failed() => ("Could not check for updates", Tone::Bad),
            Some(UpdateAction::Both) => ("Desktop and phux updates are ready", Tone::Ready),
            Some(UpdateAction::Desktop { staged: true }) => {
                ("Desktop is downloaded and verified", Tone::Ready)
            }
            Some(UpdateAction::Desktop { staged: false }) => {
                ("A Desktop update is ready", Tone::Ready)
            }
            Some(UpdateAction::Phux { retry: true }) => (
                "phux is installed; the server handoff is pending",
                Tone::Ready,
            ),
            Some(UpdateAction::Phux { retry: false }) => ("A phux update is ready", Tone::Ready),
            None if self.target.is_some() => ("Everything is up to date", Tone::Good),
            None => ("Not checked yet", Tone::Quiet),
        };
        (headline.0.to_owned(), headline.1)
    }

    /// What one click does, said once under the headline.
    pub(super) fn summary_detail(&self) -> Option<String> {
        if self.busy {
            return None;
        }
        if self.development {
            return Some(
                "Launched from a checkout or with an explicit server, so nothing installs from here."
                    .to_owned(),
            );
        }
        let text = match self.action() {
            _ if self.update_failed() => "Whatever was already in place keeps running.",
            _ if self.failed() => "Nothing was installed. The details are below.",
            Some(UpdateAction::Both) if self.staged.is_some() => {
                "Desktop is downloaded. Updates phux and hands its server off, then restarts. Sessions keep running."
            }
            Some(UpdateAction::Both) => {
                "Downloads Desktop, updates phux and hands its server off, then restarts. Sessions keep running."
            }
            Some(UpdateAction::Desktop { staged: true }) => {
                "Restart now, or it installs the next time you quit. Sessions keep running."
            }
            Some(UpdateAction::Desktop { staged: false }) => {
                "Downloads and verifies the signed bundle, then restarts. Sessions keep running."
            }
            Some(UpdateAction::Phux { retry: true }) => {
                "The new phux is in place. Retry hands the live server off without downloading again."
            }
            Some(UpdateAction::Phux { retry: false }) => {
                "Installs the verified release and hands the live server off. No session closes."
            }
            None if self.target.is_none() => return None,
            None if self.prefs.automatic => "Checked at launch and every six hours.",
            None => "Automatic checks are off.",
        };
        Some(text.to_owned())
    }

    /// A check failed for either product.
    pub(super) fn failed(&self) -> bool {
        failure(&self.message, "Desktop:").is_some() || failure(&self.message, "phux:").is_some()
    }

    /// A download or install, not a check, failed.
    pub(super) fn update_failed(&self) -> bool {
        self.failed() && self.message.contains("Update failed")
    }

    /// Failures first, then what the button cannot do by itself: a refused
    /// handoff, a protocol change left unresolved, a package-managed phux, or
    /// a Linux desktop that reruns its installer. Each with whether it failed.
    pub(super) fn notices(&self) -> Vec<(String, bool)> {
        let mut notices = ["Desktop", "phux"]
            .into_iter()
            .filter_map(|product| {
                failure(&self.message, &format!("{product}:"))
                    .map(|text| (format!("{product}: {text}"), true))
            })
            .collect::<Vec<_>>();
        if self.development || self.busy {
            return notices;
        }
        if let Some(message) = self.message.strip_prefix(HANDOFF_NOTE) {
            notices.push((message.trim().to_owned(), false));
        }
        if let Some((now, next)) = self.unresolved_protocol() {
            notices.push((
                format!(
                    "The new Desktop speaks wire protocol {next}; phux on this computer speaks {now}. Update phux too, or Desktop cannot attach after it restarts."
                ),
                false,
            ));
        }
        let packaged = self
            .runtime
            .as_ref()
            .filter(|runtime| self.prefs.manage_runtime && runtime.available && !runtime.mutable);
        if let Some(command) = packaged.and_then(|runtime| runtime.native_command.as_deref()) {
            notices.push((
                format!("phux came from a package manager, so update it there: {command}"),
                false,
            ));
        }
        if cfg!(target_os = "linux") && self.desktop_available() {
            notices.push((
                updates::linux_install_remedy(self.prefs.desktop, self.bundle.as_deref()),
                false,
            ));
        }
        notices
    }
}
