//! Adding a machine: the machines on this computer's tailnet, signing in to
//! Tailscale, and `phux host add` with its progress. The CLI does the work;
//! the dialog shows where it stands.

use super::*;

/// How often the dialog re-reads Tailscale while a sign-in or connection
/// is pending.
const TAILNET_POLL: Duration = Duration::from_secs(2);

/// How long the dialog keeps polling an unfinished sign-in.
const TAILNET_POLL_LIMIT: Duration = Duration::from_secs(15 * 60);

/// What the Add Machine dialog knows about the tailnet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum TailnetView {
    /// Not read yet.
    #[default]
    Unknown,
    /// The first read is running.
    Loading,
    /// What the CLI reported.
    Ready(host::Tailnet),
    /// The CLI could not report (missing, too old, failed).
    Failed(String),
}

/// The Add Machine dialog's state, kept while it closes and reopens so a
/// running setup stays visible.
#[derive(Debug)]
pub(crate) struct AddMachine {
    /// The target of the `phux host add` running now.
    pub(crate) adding: Option<String>,
    /// Its latest progress line.
    pub(crate) progress: Option<String>,
    /// Why the last setup failed, until the next attempt.
    pub(crate) error: Option<String>,
    /// Install or update phux on the machine when needed.
    pub(crate) install: bool,
    pub(crate) tailnet: TailnetView,
    /// A `host discover --login` is running.
    pub(crate) signing_in: bool,
    /// A read is running; a poll tick waits for it rather than racing it.
    reading: bool,
    /// The sign-in link already opened in the browser, so a poll that reads
    /// it again does not open another tab.
    opened_auth_url: Option<String>,
    /// When the current poll loop gives up; `None` when none runs.
    polling_until: Option<Instant>,
    /// Which poll loop is current; an older one stops at its next tick.
    poll_generation: u64,
}

impl Default for AddMachine {
    fn default() -> Self {
        Self {
            adding: None,
            progress: None,
            error: None,
            install: true,
            tailnet: TailnetView::Unknown,
            signing_in: false,
            reading: false,
            opened_auth_url: None,
            polling_until: None,
            poll_generation: 0,
        }
    }
}

impl AddMachine {
    /// The read tailnet, when there is one.
    pub(crate) const fn tailnet(&self) -> Option<&host::Tailnet> {
        match &self.tailnet {
            TailnetView::Ready(tailnet) => Some(tailnet),
            _ => None,
        }
    }

    /// Whether the client is somewhere a re-read can see it move on: a
    /// sign-in link waiting in the browser, or a connection coming up.
    fn pending(&self) -> bool {
        self.signing_in
            || self.tailnet().is_some_and(|tailnet| {
                matches!(tailnet.state.as_str(), "starting" | "needs_machine_auth")
                    || (tailnet.state == "needs_login" && tailnet.auth_url.is_some())
            })
    }
}

impl Desk {
    /// The dialog opened: clear a finished attempt's outcome and read the
    /// tailnet again.
    pub(crate) fn prepare_add_machine(&mut self, cx: &mut Context<Self>) {
        if self.add_machine.adding.is_none() {
            self.add_machine.progress = None;
            self.add_machine.error = None;
        }
        self.refresh_tailnet(false, cx);
    }

    /// Read the tailnet in the background; `login` first signs Tailscale in
    /// (or reconnects it) and opens the sign-in link in the browser.
    pub(crate) fn refresh_tailnet(&mut self, login: bool, cx: &mut Context<Self>) {
        if login {
            if self.add_machine.signing_in {
                return;
            }
            self.add_machine.signing_in = true;
        }
        self.add_machine.reading = true;
        if matches!(
            self.add_machine.tailnet,
            TailnetView::Unknown | TailnetView::Failed(_)
        ) {
            self.add_machine.tailnet = TailnetView::Loading;
        }
        let phux = self.phux.clone();
        let read = cx
            .background_executor()
            .spawn(async move { host::discover_tailnet(&phux, login) });
        cx.spawn(async move |desk, cx| {
            let result = read.await;
            let _ = desk.update(cx, |desk, cx| desk.tailnet_read(login, result, cx));
        })
        .detach();
        cx.notify();
    }

    fn tailnet_read(
        &mut self,
        login: bool,
        result: Result<host::Tailnet, String>,
        cx: &mut Context<Self>,
    ) {
        self.add_machine.reading = false;
        if login {
            self.add_machine.signing_in = false;
        }
        match result {
            Ok(tailnet) => {
                if let Some(url) = tailnet.auth_url.clone()
                    && (login || self.add_machine.opened_auth_url.is_some())
                    && self.add_machine.opened_auth_url.as_ref() != Some(&url)
                {
                    cx.open_url(&url);
                    self.add_machine.opened_auth_url = Some(url);
                }
                if tailnet.running() {
                    self.add_machine.opened_auth_url = None;
                }
                self.add_machine.tailnet = TailnetView::Ready(tailnet);
            }
            // A failed sign-in keeps what was read before and says why.
            Err(error) if login => {
                self.post(Kind::Error, "Could not sign in to Tailscale", Some(error));
            }
            // A poll that fails keeps the last good reading on screen.
            Err(error) if self.add_machine.tailnet().is_some() => {
                log::warn!("tailnet read: {error}");
            }
            Err(error) => self.add_machine.tailnet = TailnetView::Failed(error),
        }
        self.poll_tailnet(cx);
        cx.notify();
    }

    /// Keep re-reading while the dialog is open and something is pending.
    fn poll_tailnet(&mut self, cx: &mut Context<Self>) {
        if self.modal != Modal::AddHost || !self.add_machine.pending() {
            self.add_machine.polling_until = None;
            return;
        }
        if self.add_machine.polling_until.is_some() {
            return;
        }
        self.add_machine.polling_until = Some(Instant::now() + TAILNET_POLL_LIMIT);
        self.add_machine.poll_generation += 1;
        let generation = self.add_machine.poll_generation;
        cx.spawn(async move |desk, cx| {
            loop {
                cx.background_executor().timer(TAILNET_POLL).await;
                let again = desk.update(cx, |desk, cx| {
                    let live = desk.add_machine.poll_generation == generation
                        && desk.modal == Modal::AddHost
                        && desk.add_machine.pending()
                        && desk
                            .add_machine
                            .polling_until
                            .is_some_and(|until| Instant::now() < until);
                    if live && !desk.add_machine.signing_in && !desk.add_machine.reading {
                        desk.refresh_tailnet(false, cx);
                    }
                    live
                });
                if !matches!(again, Ok(true)) {
                    let _ = desk.update(cx, |desk, _| {
                        if desk.add_machine.poll_generation == generation {
                            desk.add_machine.polling_until = None;
                        }
                    });
                    break;
                }
            }
        })
        .detach();
    }

    /// Fill the dialog with a machine from the tailnet.
    pub(crate) fn choose_peer(
        &mut self,
        peer: &host::TailnetPeer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (slot, value) in [
            (InputSlot::HostTarget, peer.ssh_host.clone()),
            (InputSlot::HostName, peer.name.clone()),
        ] {
            if let Some(input) = self.input_entity(slot) {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
        self.add_machine.error = None;
        cx.notify();
    }

    pub(crate) fn set_install(&mut self, install: bool, cx: &mut Context<Self>) {
        self.add_machine.install = install;
        cx.notify();
    }

    /// Enroll `target` over ssh with `phux host add`. The dialog stays open
    /// and shows each step; closing it leaves the setup running, and a
    /// notification reports how it ended.
    pub(crate) fn apply_add_host(
        &mut self,
        target: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = target.trim().to_owned();
        if target.is_empty() {
            self.add_machine.error = Some("Enter an ssh destination such as me@mini.".into());
            cx.notify();
            return;
        }
        if self.add_machine.adding.is_some() {
            return;
        }
        // The tailnet authenticates its peers' addresses, so ssh may record
        // a first-seen host key for one; any other target keeps strict
        // checking.
        let accept_new_host_key = self
            .add_machine
            .tailnet()
            .is_some_and(|tailnet| tailnet.peer_for(&target).is_some());
        let request = host::AddHost {
            target: target.clone(),
            name: name.trim().to_owned(),
            install: self.add_machine.install,
            accept_new_host_key,
        };
        let label = if request.name.is_empty() {
            target.clone()
        } else {
            request.name.clone()
        };
        self.add_machine.adding = Some(target);
        self.add_machine.progress = Some("Connecting over ssh…".into());
        self.add_machine.error = None;
        let phux = self.phux.clone();
        let (steps, progress) = smol::channel::unbounded::<String>();
        let setup = cx.background_executor().spawn(async move {
            host::add_host(&phux, &request, |step| {
                let _ = steps.send_blocking(step);
            })
        });
        cx.spawn_in(window, async move |desk, cx| {
            while let Ok(step) = progress.recv().await {
                let shown = desk.update(cx, |desk, cx| {
                    desk.add_machine.progress = Some(step);
                    cx.notify();
                });
                if shown.is_err() {
                    break;
                }
            }
            let result = setup.await;
            let _ = desk.update_in(cx, |desk, window, cx| {
                desk.add_machine.adding = None;
                desk.add_machine.progress = None;
                match result {
                    Ok(warnings) => {
                        let body = (!warnings.is_empty()).then(|| warnings.join("\n"));
                        desk.post(Kind::Info, &format!("Added {label}"), body);
                        if desk.modal == Modal::AddHost {
                            desk.close_modal(window, cx);
                        }
                    }
                    Err(error) => {
                        desk.add_machine.error = Some(error.clone());
                        desk.post(Kind::Error, &format!("Could not add {label}"), Some(error));
                    }
                }
                desk.refresh_hosts(cx);
                if desk.modal == Modal::AddHost {
                    desk.refresh_tailnet(false, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
