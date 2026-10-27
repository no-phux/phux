//! `phux host minimal` — Minimal (<https://minimal.dev>) boxes as satellites
//! of this hub (ADR-0182's inbound join, delivered for the box).
//!
//! A box is a Linux microVM or namespace whose loopback is its own, so the
//! hub cannot dial it; the box can reach this machine's loopback as
//! `host.min.internal`. `setup` writes a Minimal loadout that patches a Linux
//! `phux` into every box and starts its server on activation, makes that
//! loadout a default, and installs a per-user service running `watch`.
//! `sync` (and `watch`, its loop) mints a single-use ticket for each running
//! box that has not joined and hands the join code to `phux host join -`
//! inside the box over `min session exec`'s stdin, so the code is never in a
//! file or an argv. A destroyed box's satellite entry and credential go with
//! it. Minimal has no host-side hook, which is why the hub watches.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use usage::{Args, Subcommands};

use super::super::json_err::{self, CliError, codes};
use super::super::satellite as satellite_registry;
use super::inbound;

mod boxes;
mod files;
mod forget;
mod hub;
mod memory;
mod service;
mod session;
#[cfg(test)]
mod tests;

use boxes::{BoxSession, MinCli};
use files::{BoxRegistry, Joined, MinimalDirs};
use forget::forget_box;
use memory::Memory;

/// The loadout `setup` writes and adds to `default_loadouts`.
const LOADOUT: &str = "phux";

/// Every satellite this integration registers is named `min-…`.
const SATELLITE_PREFIX: &str = "min-";

/// Minutes a join code minted for a box stays redeemable; it is spent within
/// seconds, so this only bounds a failed delivery.
const TICKET_MINUTES: i64 = 5;

/// `phux host minimal <action>`.
#[derive(Debug, Subcommands)]
pub(crate) enum MinimalAction {
    /// Make every Minimal box join this hub.
    ///
    /// Writes the `phux` Minimal loadout, which copies a Linux phux into
    /// each box and starts its server, adds it to Minimal's
    /// `default_loadouts`, makes this machine's service a federation hub,
    /// and installs a per-user service running `phux host minimal watch`.
    /// On macOS the box binary is this phux's own version for Linux,
    /// downloaded and signature-checked like `phux update`, and replaced
    /// when the cached copy is a different version. Run it again after the
    /// hub's QUIC port changes. The hub must run `[policy] mode = "paired"`
    /// with a QUIC listener on loopback or all addresses.
    Setup {
        #[usage(flatten)]
        opts: MinimalOpts,

        /// The Linux phux to copy into boxes, instead of this version's
        /// Linux build (macOS) or this binary (Linux).
        #[usage(long, value_name = "PATH")]
        box_phux: Option<PathBuf>,

        /// Skip the watcher service; run `phux host minimal watch` yourself.
        #[usage(long)]
        no_service: bool,
    },

    /// Join each running box that has not joined, and forget destroyed ones.
    ///
    /// A box joins as satellite `min-NAME`, NAME being its Minimal session
    /// name, and gets session `min-NAME` here: one pane running `min session
    /// attach` into the box, ending when the box is destroyed. A box without
    /// phux on its `PATH` (activated without the `phux` loadout) is skipped.
    Sync {
        #[usage(flatten)]
        opts: MinimalOpts,
    },

    /// Run `sync` every few seconds until stopped; what the service runs.
    Watch {
        #[usage(flatten)]
        opts: MinimalOpts,

        /// Seconds between passes.
        #[usage(
            long,
            value_name = "SECS",
            default = "2",
            default_value_t = 2,
            validate = "int(value) >= 1 && int(value) <= 3600",
            validate_error = "must be between 1 and 3600 seconds"
        )]
        interval: u64,
    },

    /// Undo `setup`: stop the watcher, remove the loadout, forget every box.
    ///
    /// Running boxes keep their phux; they stop reaching this hub because
    /// their credentials are revoked.
    Remove,
}

/// Flags every `phux host minimal` action takes.
#[derive(Debug, Args)]
pub(crate) struct MinimalOpts {
    /// `HOST:PORT` boxes dial. Omitted: `host.min.internal` and the port of
    /// this server's live QUIC listener.
    #[usage(long, value_name = "HOST:PORT")]
    hub: Option<String>,

    /// The `min` CLI. Omitted: `min` on `PATH`, else `~/.local/bin/min`.
    #[usage(long, value_name = "PATH")]
    min: Option<PathBuf>,
}

pub(super) fn run(action: &MinimalAction) -> ExitCode {
    let result = match action {
        MinimalAction::Setup {
            opts,
            box_phux,
            no_service,
        } => setup(opts, box_phux.as_deref(), *no_service),
        MinimalAction::Sync { opts } => {
            Context::new(opts).and_then(|ctx| ctx.sync(&mut Memory::default()))
        }
        MinimalAction::Watch { opts, interval } => {
            Context::new(opts).and_then(|ctx| ctx.watch(Duration::from_secs(*interval)))
        }
        MinimalAction::Remove => remove(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => json_err::emit(false, &err, 1),
    }
}

/// What `sync` needs: the `min` CLI, the address boxes dial, and the record
/// of which box joined as which satellite.
struct Context {
    min: MinCli,
    hub: String,
    registry: BoxRegistry,
    files: inbound::HubFiles,
}

impl Context {
    fn new(opts: &MinimalOpts) -> Result<Self, CliError> {
        Ok(Self {
            min: MinCli::resolve(opts.min.as_deref()).map_err(no_min)?,
            hub: hub::endpoint(opts.hub.as_deref())?,
            registry: BoxRegistry::at(files::registry_path()),
            files: inbound::HubFiles::from_env(),
        })
    }

    /// One pass. `memory` carries what earlier `watch` passes learned, so a
    /// box is probed and reported once and a failing one is retried slowly.
    fn sync(&self, memory: &mut Memory) -> Result<(), CliError> {
        let sessions = self.min.sessions().map_err(no_min)?;
        let mut joined = self.registry.load().map_err(state_failure)?;
        let live: BTreeSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        memory.forget_gone(&live);
        self.prune(&mut joined, &live)?;
        for session in sessions.iter().filter(|s| s.is_active()) {
            if !joined.boxes.contains_key(&session.id) && !memory.waits_on(&session.id) {
                self.admit(session, &mut joined, memory)?;
            }
        }
        Ok(())
    }

    /// Forget every joined box that is no longer `live`. A revocation that
    /// fails leaves that box recorded, so the next pass retries it, and is
    /// not described as revoked.
    fn prune(&self, joined: &mut Joined, live: &BTreeSet<&str>) -> Result<(), CliError> {
        let gone: Vec<String> = joined
            .boxes
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            let Some(satellite) = joined.boxes.get(&id).cloned() else {
                continue;
            };
            forget_box(&satellite, &joined.minted, &self.files)?;
            joined.boxes.remove(&id);
            joined.minted.remove(&satellite);
            self.registry.save(joined).map_err(state_failure)?;
            outln!("{satellite}: box destroyed; removed it and revoked its credential");
        }
        Ok(())
    }

    /// Join one box that has not joined, or note why it cannot.
    fn admit(
        &self,
        session: &BoxSession,
        joined: &mut Joined,
        memory: &mut Memory,
    ) -> Result<(), CliError> {
        if !self.min.has_phux(&session.id) {
            if memory.lacks_phux(&session.id) {
                outln!(
                    "{}: box has no phux; activate it with the `{LOADOUT}` loadout to join it",
                    session.label()
                );
            }
            return Ok(());
        }
        memory.found_phux(&session.id);
        let taken: BTreeSet<&str> = joined.boxes.values().map(String::as_str).collect();
        let satellite = satellite_name(session, &taken);
        if let Err(reason) = self.join(session, &satellite, joined) {
            if memory.fail(&session.id, &reason) {
                warn!("{satellite}: could not join (retrying every 30s): {reason}");
            }
            return Ok(());
        }
        memory.joined(&session.id);
        joined.boxes.insert(session.id.clone(), satellite.clone());
        self.registry.save(joined).map_err(state_failure)?;
        outln!("{satellite}: joined (panes appear as {satellite}/@N)");
        match session::open(&self.min, session, &satellite) {
            Ok(()) => outln!("{satellite}: session {satellite} is a shell in the box"),
            Err(reason) => warn!("{satellite}: could not open its session here: {reason}"),
        }
        Ok(())
    }

    /// Mint `satellite`'s code and redeem it inside `session`'s box. A name
    /// already in the satellite registry is refused unless this feature
    /// minted it. A failed delivery withdraws the ticket and the entry it
    /// registered.
    fn join(
        &self,
        session: &BoxSession,
        satellite: &str,
        joined: &mut Joined,
    ) -> Result<(), String> {
        self.refuse_foreign(satellite, joined)?;
        let adopt = joined.minted.contains(satellite);
        let (code, ticket) =
            inbound::mint_code(satellite, &self.hub, TICKET_MINUTES, &self.files, adopt)
                .map_err(|err| err.message)?;
        joined.minted.insert(satellite.to_owned());
        if let Err(err) = self.registry.save(joined) {
            self.withdraw_failed_join(&ticket, satellite, joined);
            return Err(err);
        }
        self.min.join(&session.id, &code).inspect_err(|_| {
            self.withdraw_failed_join(&ticket, satellite, joined);
        })
    }

    /// A satellite this feature did not mint is left alone.
    fn refuse_foreign(&self, satellite: &str, joined: &Joined) -> Result<(), String> {
        if joined.minted.contains(satellite) {
            return Ok(());
        }
        let entries = satellite_registry::load_registry_from(&self.files.config)?;
        if let Some(entry) = entries.into_iter().find(|entry| entry.name == satellite) {
            return Err(format!(
                "satellite {satellite} already exists at {} and was not minted by `phux host minimal`",
                entry.endpoint
            ));
        }
        Ok(())
    }

    /// Withdraw the ticket and the entry a failed delivery registered. The
    /// name stays minted when that cleanup itself fails, so the next pass
    /// retries it and does not treat the name as someone else's.
    fn withdraw_failed_join(&self, ticket: &str, satellite: &str, joined: &mut Joined) {
        if let Err(err) = inbound::withdraw_ticket(&self.files.workload.registry, ticket) {
            warn!("{satellite}: could not withdraw ticket {ticket} ({err})");
        }
        match forget_box(satellite, &joined.minted, &self.files) {
            Ok(()) => {
                joined.minted.remove(satellite);
                if let Err(err) = self.registry.save(joined) {
                    warn!("{satellite}: could not record that its join was withdrawn ({err})");
                }
            }
            Err(err) => warn!(
                "{satellite}: could not forget the failed join ({})",
                err.message
            ),
        }
    }

    fn watch(&self, interval: Duration) -> Result<(), CliError> {
        info!(
            "watching Minimal boxes every {}s; boxes dial {}",
            interval.as_secs(),
            self.hub
        );
        let mut memory = Memory::default();
        let mut last_error = None;
        loop {
            // A pass that fails (Minimal's daemon restarting, a config being
            // rewritten) is reported once and retried, never fatal.
            match self.sync(&mut memory) {
                Ok(()) => last_error = None,
                Err(err) if last_error.as_ref() != Some(&err.message) => {
                    warn!("{}", err.message);
                    last_error = Some(err.message);
                }
                Err(_) => {}
            }
            std::thread::sleep(interval);
        }
    }
}

/// `min-NAME` for a box, NAME being its session name (else its id's first
/// eight characters) with each character a satellite name refuses replaced.
/// A name `taken` by another box gets the id prefix appended.
fn satellite_name(session: &BoxSession, taken: &BTreeSet<&str>) -> String {
    let short_id: String = session.id.chars().take(8).collect();
    let base = session
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&short_id);
    let clean: String = base
        .chars()
        .map(|c| match c {
            '/' | ':' => '-',
            c if c.is_control() || c.is_whitespace() => '-',
            c => c,
        })
        .take(200)
        .collect();
    let name = format!("{SATELLITE_PREFIX}{clean}");
    if taken.contains(name.as_str()) {
        format!("{name}-{short_id}")
    } else {
        name
    }
}

fn setup(opts: &MinimalOpts, box_phux: Option<&Path>, no_service: bool) -> Result<(), CliError> {
    let ctx = Context::new(opts)?;
    install_loadout(&box_binary(box_phux)?)?;
    report_local_hub(&super::super::service::ensure_local_hub());
    let unit = if no_service {
        None
    } else {
        Some(service::install(&ctx.min, opts.hub.as_deref()).map_err(setup_failure)?)
    };
    match &unit {
        Some(unit) => outln!("Watcher service installed: {}", shown(unit)),
        None => outln!("Skipped the watcher service; run `phux host minimal watch` to join boxes."),
    }
    outln!();
    outln!(
        "Boxes dial {}. Every `min session activate` now joins this hub:",
        ctx.hub
    );
    outln!("  phux ls          lists each box's sessions under min-NAME");
    outln!("  phux attach min-NAME/@1, or the desktop app, opens its panes");
    // The service's first pass joins the boxes already running; a second
    // pass here would race it for the same names.
    if unit.is_some() {
        return Ok(());
    }
    outln!();
    ctx.sync(&mut Memory::default())
}

/// Write the loadout patching `binary` into boxes and make it a default.
fn install_loadout(binary: &Path) -> Result<(), CliError> {
    let dirs = MinimalDirs::from_env().map_err(setup_failure)?;
    dirs.write_loadout(LOADOUT, binary).map_err(setup_failure)?;
    outln!(
        "Wrote the `{LOADOUT}` Minimal loadout: {}",
        shown(&dirs.loadout_file(LOADOUT))
    );
    outln!("  boxes get {}", shown(binary));
    if dirs.add_default_loadout(LOADOUT).map_err(setup_failure)? {
        outln!(
            "Added `{LOADOUT}` to default_loadouts in {}",
            shown(&dirs.config_file())
        );
    }
    Ok(())
}

fn report_local_hub(hub: &super::super::service::LocalHub) {
    use super::super::service::LocalHub;
    match hub {
        LocalHub::Already => outln!("This machine's phux service already runs as a hub."),
        LocalHub::Patched => outln!(
            "Made this machine's phux service a hub; it takes effect when that server next starts."
        ),
        LocalHub::Installed => outln!(
            "Wrote a hub service for this machine; phux starts it once no server is running."
        ),
        LocalHub::Skipped(reason) => warn!(
            "could not make this machine's phux service a hub ({reason}); run the server with --hub"
        ),
    }
}

fn remove() -> Result<(), CliError> {
    finish_remove(
        service::uninstall(),
        MinimalDirs::from_env,
        &BoxRegistry::at(files::registry_path()),
        &inbound::HubFiles::from_env(),
    )
}

/// Undo the watcher, the loadout, and every minted satellite. An uninstall
/// failure stops the rest: the loadout and the credentials stay until a
/// later `remove` can finish.
fn finish_remove(
    uninstalled: Result<Option<PathBuf>, String>,
    dirs: impl FnOnce() -> Result<MinimalDirs, String>,
    registry: &BoxRegistry,
    files: &inbound::HubFiles,
) -> Result<(), CliError> {
    match uninstalled {
        Ok(Some(unit)) => outln!("Removed the watcher service {}", shown(&unit)),
        Ok(None) => {}
        Err(err) => {
            return Err(setup_failure(format!(
                "could not remove the watcher service: {err}"
            )));
        }
    }
    let dirs = dirs().map_err(setup_failure)?;
    if dirs.remove_loadout(LOADOUT).map_err(setup_failure)? {
        outln!("Removed the `{LOADOUT}` Minimal loadout and its default_loadouts entry");
    }
    let mut joined = registry.load().map_err(state_failure)?;
    let names: Vec<String> = joined.minted.iter().cloned().collect();
    for satellite in names {
        forget_box(&satellite, &joined.minted, files)?;
        joined.boxes.retain(|_, name| name != &satellite);
        joined.minted.remove(&satellite);
        registry.save(&joined).map_err(state_failure)?;
        outln!("{satellite}: removed and revoked");
    }
    Ok(())
}

/// The Linux phux boxes get: `--box-phux`, this binary on Linux, else this
/// phux's own version for the machine's architecture.
fn box_binary(explicit: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = explicit {
        return std::path::absolute(path)
            .ok()
            .filter(|path| path.is_file())
            .ok_or_else(|| setup_failure(format!("--box-phux {} is not a file", path.display())));
    }
    if cfg!(target_os = "linux") {
        return std::env::current_exe()
            .map_err(|err| setup_failure(format!("could not resolve this binary: {err}")));
    }
    let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
    files::fetch_box_binary(&target).map_err(setup_failure)
}

/// `path` with the home directory spelled `~`, for messages.
fn shown(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
        .map_or_else(
            || path.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
}

fn no_min(detail: String) -> CliError {
    CliError::new(
        codes::NO_SERVER,
        detail,
        "install Minimal (curl -fsSL https://go.minimal.dev/stable | sh), or pass --min PATH",
    )
}

fn setup_failure(detail: String) -> CliError {
    CliError::new(
        codes::INTERNAL_ERROR,
        detail,
        "check the path's permissions, then rerun `phux host minimal setup`",
    )
}

fn state_failure(detail: impl std::fmt::Display) -> CliError {
    CliError::new(
        codes::INTERNAL_ERROR,
        format!("could not use the joined-box record: {detail}"),
        "check the state directory's permissions, then rerun",
    )
}
