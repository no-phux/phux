//! Pair a phone: the code on screen, the devices that can connect, and the
//! `phux pair` calls behind them. The words and parsing
//! live in `crate::pair`; the view in `ui::pair`.
//!
//! ponytail: like onboarding, always this computer's server, even with the
//! window on another machine.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use rune::Tx;

use super::super::{Desk, NoticeKind, SettingsCategory};
use crate::pair::{self, Code, Device};

const QUICK: Duration = Duration::from_secs(15);
/// `--enable` writes paired mode and waits for the server to restart.
const SLOW: Duration = Duration::from_secs(60);
/// How often the device list is read while a code is up, in seconds, so a
/// phone that pairs shows up on its own.
const WATCH_EVERY: f64 = 3.0;
/// The QR's smallest side in pixels: crisp at its 2x display size.
pub const QR_PIXELS: u32 = 440;
/// Where a computer with no tailnet gets one.
const TAILSCALE: &str = "https://tailscale.com/download";

enum Done {
    Code(Result<Code, String>),
    Devices(Result<Vec<Device>, String>),
    Revoked(String, Result<(), String>),
}

pub struct Pairing {
    /// The code on screen, or why there is none; `None` before one is asked for.
    pub code: Option<Result<Code, String>>,
    pub qr: Option<rune::Image>,
    pub minting: bool,
    /// The devices that can connect; `None` before they are listed.
    pub devices: Option<Result<Vec<Device>, String>>,
    pub copied: bool,
    listed_at: f64,
    listing: bool,
    done: (Sender<Done>, Receiver<Done>),
}

impl Default for Pairing {
    fn default() -> Self {
        Self {
            code: None,
            qr: None,
            minting: false,
            devices: None,
            copied: false,
            listed_at: f64::NEG_INFINITY,
            listing: false,
            done: channel(),
        }
    }
}

impl Pairing {
    /// The code still pairs: shown, and not run out.
    pub fn live_code(&self, now: i64) -> Option<&Code> {
        match &self.code {
            Some(Ok(code)) if code.expires_at.is_none_or(|at| at > now) => Some(code),
            _ => None,
        }
    }
}

impl Desk {
    /// Run `phux` with `args` off the main thread; `server` names this
    /// computer's server, which only a mint dials.
    fn pair_ask(
        &self,
        args: &[&str],
        server: bool,
        timeout: Duration,
        done: impl FnOnce(Result<Vec<u8>, String>) -> Done + Send + 'static,
    ) {
        let sender = self.settings_ui.pairing.done.0.clone();
        self.ask_phux(sender, server, args, timeout, done);
    }

    /// A fresh code. Never opens the server to anything new: that takes
    /// [`Desk::allow_phones`].
    pub fn new_pair_code(&mut self) {
        self.mint_pair_code(false);
    }

    /// The explicit choice to let phones connect: paired mode, on the
    /// addresses the server binds for it (the tailnet), then a code.
    pub fn allow_phones(&mut self) {
        self.mint_pair_code(true);
    }

    fn mint_pair_code(&mut self, enable: bool) {
        if std::mem::replace(&mut self.settings_ui.pairing.minting, true) {
            return;
        }
        self.settings_ui.pairing.copied = false;
        let mut args = vec!["pair", "--json", "--enroll"];
        if enable {
            args.push("--enable");
        }
        let timeout = if enable { SLOW } else { QUICK };
        self.pair_ask(&args, true, timeout, |out| {
            Done::Code(out.and_then(|bytes| pair::parse_code(&bytes)))
        });
        self.list_devices();
    }

    pub fn list_devices(&mut self) {
        if std::mem::replace(&mut self.settings_ui.pairing.listing, true) {
            return;
        }
        self.settings_ui.pairing.listed_at = self.clock;
        self.pair_ask(&["pair", "ls", "--json"], false, QUICK, |out| {
            Done::Devices(out.and_then(|bytes| pair::parse_devices(&bytes)))
        });
    }

    pub fn revoke_device(&mut self, id: String) {
        let target = id.clone();
        self.pair_ask(
            &["pair", "revoke", &target, "--json"],
            false,
            QUICK,
            move |out| Done::Revoked(id, out.map(|_| ())),
        );
    }

    pub fn copy_pair_link(&mut self, tx: &mut Tx) {
        if let Some(Ok(code)) = &self.settings_ui.pairing.code {
            tx.copy(code.link.clone());
            self.settings_ui.pairing.copied = true;
        }
    }

    /// The CLI's words behind a refusal the card says in a sentence.
    pub fn copy_pair_details(&mut self, tx: &mut Tx) {
        if let Some(Err(error)) = &self.settings_ui.pairing.code {
            tx.copy(error.clone());
            self.settings_ui.pairing.copied = true;
        }
    }

    /// Tailscale's download page, where installing starts its sign-in.
    pub fn get_tailscale(&mut self, tx: &mut Tx) {
        if let Err(error) = tx.open_url(TAILSCALE) {
            self.say(
                NoticeKind::Error,
                format!("Couldn't open {TAILSCALE}: {error}"),
            );
        }
    }

    /// Back at the window after setting up Tailscale: ask again, so the
    /// code appears without another press. Never turns anything on.
    pub(crate) fn recheck_pairing(&mut self) {
        let waiting = matches!(&self.settings_ui.pairing.code,
            Some(Err(error)) if pair::no_address(error));
        if waiting && self.pairing_shown() {
            self.new_pair_code();
        }
    }

    /// From the palette: Settings at Remote Hosts, with a code up.
    pub fn open_pairing(&mut self) {
        if self.phone.is_some() {
            return;
        }
        self.close_inbox();
        self.settings_open = true;
        self.settings_category(SettingsCategory::RemoteHosts);
        if self.settings_ui.pairing.live_code(pair::now()).is_none() {
            self.new_pair_code();
        }
    }

    fn pairing_shown(&self) -> bool {
        self.settings_open || self.onboarding.open
    }

    /// Answers from the CLI, the countdown's tick, and the watch for a phone
    /// that pairs. True when anything on screen changed.
    pub(super) fn pump_pairing(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        while let Ok(done) = self.settings_ui.pairing.done.1.try_recv() {
            changed = true;
            match done {
                Done::Code(result) => {
                    self.settings_ui.pairing.minting = false;
                    self.settings_ui.pairing.qr = result.as_ref().ok().and_then(|code| {
                        let cache = self.connection.resources.images.as_ref()?;
                        let (side, rgba) = pair::qr_rgba(&code.link, QR_PIXELS)?;
                        Some(cache.rgba(side, side, rgba))
                    });
                    self.settings_ui.pairing.code = Some(result);
                }
                Done::Devices(result) => {
                    self.settings_ui.pairing.listing = false;
                    // The code's own credential has connected: a phone used it.
                    let paired = match (self.settings_ui.pairing.live_code(pair::now()), &result) {
                        (Some(code), Ok(devices)) => devices
                            .iter()
                            .any(|d| d.id == code.credential && d.last_seen.is_some()),
                        _ => false,
                    };
                    if paired {
                        // The code is single use: it did its job.
                        self.settings_ui.pairing.code = None;
                        self.settings_ui.pairing.qr = None;
                        self.say(NoticeKind::Info, "Phone paired.");
                    }
                    self.settings_ui.pairing.devices = Some(result);
                }
                Done::Revoked(_, Ok(())) => {
                    self.say(
                        NoticeKind::Info,
                        "Revoked. That device can't connect again; one already connected \
                         stays until it disconnects.",
                    );
                    self.list_devices();
                }
                Done::Revoked(id, Err(_)) => {
                    let name = Device {
                        id,
                        last_seen: None,
                    }
                    .name();
                    self.say(
                        NoticeKind::Error,
                        format!("Couldn't revoke {name}. Try again."),
                    );
                }
            }
        }
        if !self.pairing_shown() {
            return changed;
        }
        let now = pair::now();
        let watching = self.settings_ui.pairing.live_code(now).is_some();
        if watching
            && !self.settings_ui.pairing.listing
            && self.clock - self.settings_ui.pairing.listed_at >= WATCH_EVERY
        {
            self.list_devices();
        }
        // The countdown moves once a second, through to showing it ran out.
        let counting = matches!(&self.settings_ui.pairing.code,
            Some(Ok(Code { expires_at: Some(at), .. })) if now <= at + 1);
        if watching || counting {
            tx.wake_after(Duration::from_secs(1));
        }
        changed | counting
    }
}

#[cfg(test)]
impl Desk {
    /// A code up for nine minutes on the tailnet, and two paired devices,
    /// as a fixture render shows them. The QR is decoded before it returns.
    pub(crate) fn pairing_fixture(&mut self) {
        let link = "https://phux.sh/connect?url=wss://studio.tail01774c.ts.net:8787\
                    &quic=quic://studio.tail01774c.ts.net:8788&fp=AA:AE:09:28:90:59:26:49\
                    #enroll=0123456789abcdef0123456789abcdef&token=fedcba9876543210fedcba9876543210";
        let now = pair::now();
        self.settings_ui.pairing.code = Some(Ok(Code {
            link: link.to_owned(),
            expires_at: Some(now + 542),
            once: true,
            credential: "00f52232cc292e87b6695a8b67d363f6".to_owned(),
            network: pair::Network::Tailnet,
        }));
        self.settings_ui.pairing.qr = self.connection.resources.images.as_ref().map(|cache| {
            let (side, rgba) = pair::qr_rgba(link, QR_PIXELS).unwrap();
            let image = cache.rgba(side, side, rgba);
            image.wait().unwrap();
            image
        });
        self.settings_ui.pairing.devices = Some(Ok(vec![
            Device {
                id: "84b11329e12c1d8ee514eb29e5b390d8".to_owned(),
                last_seen: Some(now - 1200),
            },
            Device {
                id: "5df67277173a46c6b482089d2743ed59".to_owned(),
                last_seen: Some(now - 86_400 * 4),
            },
        ]));
    }
}
