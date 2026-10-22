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
            (Work::Downloading, false) => self.phux_due().then_some(("On restart", Tone::Quiet)),
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
            (
                if self.staged.is_some() {
                    "On restart"
                } else {
                    "Update ready"
                },
                Tone::Ready,
            )
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
        if self.update_failed() {
            return ("The update did not finish".to_owned(), Tone::Bad);
        }
        if self.failed() {
            return ("Could not check for updates".to_owned(), Tone::Bad);
        }
        if self.pending_handoff {
            return ("Server handoff needs another try".to_owned(), Tone::Ready);
        }
        let headline = self.resting_summary();
        (headline.0.to_owned(), headline.1)
    }

    fn resting_summary(&self) -> (&'static str, Tone) {
        match self.action() {
            Some(UpdateAction::Both) if self.staged.is_some() => {
                ("Ready when you are", Tone::Ready)
            }
            Some(action) => (ready_headline(action), Tone::Ready),
            None if self.completed => ("Local server updated", Tone::Good),
            None if self.runtime_attention() => ("A server update is available", Tone::Ready),
            None if self.target.is_some() => ("Everything is up to date", Tone::Good),
            None => ("Not checked yet", Tone::Quiet),
        }
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
        if self.update_failed() {
            return Some("Whatever was already in place keeps running.".to_owned());
        }
        if self.failed() {
            return Some("Nothing was installed. The details are below.".to_owned());
        }
        if let Some(action) = self.action() {
            return Some(self.action_detail(action).to_owned());
        }
        if self.completed {
            return Some("Your sessions keep running. No Desktop restart is needed.".to_owned());
        }
        if self.runtime_attention() {
            return Some(
                "phux is managed outside Desktop. Use its package manager to update.".to_owned(),
            );
        }
        self.target.as_ref()?;
        Some(
            if self.prefs.automatic {
                "Checked at launch and every six hours."
            } else {
                "Automatic checks are off."
            }
            .to_owned(),
        )
    }

    fn action_detail(&self, action: UpdateAction) -> &'static str {
        match action {
            UpdateAction::Both if self.staged.is_some() => {
                "Desktop is verified. Restart updates the local server first, then reopens Desktop. Sessions keep running."
            }
            UpdateAction::Both => {
                "Downloads and verifies Desktop. The local server updates when you choose to restart. Sessions keep running."
            }
            UpdateAction::Desktop { staged: true } => {
                "Restart now, or it installs the next time you quit. Sessions keep running."
            }
            UpdateAction::Desktop { staged: false } => {
                "Downloads and verifies the signed bundle. Restart when you’re ready; sessions keep running."
            }
            UpdateAction::Phux { retry: true } => {
                "The new phux is in place. Retry hands the live server off without downloading again."
            }
            UpdateAction::Phux { retry: false } => {
                "Installs the verified release and hands the live server off. No session closes."
            }
        }
    }

    /// No nagging when current or in development; a completed handoff stays dismissible.
    pub(super) fn has_card(&self) -> bool {
        if self.development {
            return false;
        }
        self.desktop_due()
            || self.runtime_attention()
            || self.failed()
            || self.completed
            || (self.busy && self.work != Work::Checking)
    }

    fn runtime_attention(&self) -> bool {
        self.prefs.manage_runtime
            && (self.pending_handoff
                || self
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.available))
    }

    pub(super) fn action_label(&self) -> Option<&'static str> {
        let action = self.action()?;
        if cfg!(target_os = "linux") && action.desktop() {
            return Some("Installation instructions");
        }
        if self.pending_handoff {
            if self.staged.is_some() {
                return Some("Retry & restart");
            }
            return Some("Retry server handoff");
        }
        if action == UpdateAction::Both && self.staged.is_some() {
            return Some("Restart & update server");
        }
        Some(action.label())
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

fn ready_headline(action: UpdateAction) -> &'static str {
    match action {
        UpdateAction::Both => "Desktop and server updates are ready",
        UpdateAction::Desktop { staged: true } => "Ready when you are",
        UpdateAction::Desktop { staged: false } => "A Desktop update is ready",
        UpdateAction::Phux { retry: true } => "Server handoff needs another try",
        UpdateAction::Phux { retry: false } => "A server update is ready",
    }
}
