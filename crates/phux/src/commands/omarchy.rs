//! `phux omarchy <action>`: fit phux into an Omarchy desktop.
//!
//! Omarchy (<https://omarchy.org>) wires its multiplexers into the desktop
//! in a handful of places: a key that opens one, a key that lists its
//! bindings, a server started with the graphical session, keys that match
//! its tmux config, and a hook that repaints them when the theme changes.
//! `setup` puts phux in each of those places, `remove` takes it out again,
//! and `status` says which are in place. Every write is a file phux owns
//! outright or a marked block inside a file it shares with the user.

mod blocks;
mod hyprland;
mod keys;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::distro::{current_distro, switch_distro};
use phux_config::loader as config_loader;
use phux_config::settings::Edit;
use phux_config::theme::{FILE_KEY, ThemeSelection};

use self::hyprland::Hyprland;
use crate::commands::OmarchyAction;

/// The layer `setup` installs as the selected distro, after its `extends`.
const LAYER: &str = include_str!("omarchy/omarchy.toml");

/// Omarchy's current theme, which it swaps on every theme change.
const THEME_FILE: &str = "~/.local/state/omarchy/current/theme/colors.toml";

/// Runs after every Omarchy theme change (`omarchy-hook theme-set`).
const HOOK: &str = "#!/bin/bash\n\
# Written by `phux omarchy setup` to repaint phux in the new Omarchy theme.\n\
# `phux omarchy remove` deletes it.\n\
phux={PHUX}\n\
[[ -x $phux ]] || phux=$(command -v phux) || exit 0\n\
\"$phux\" config reload >/dev/null 2>&1 || true\n";

pub(crate) fn run_omarchy(action: &OmarchyAction, socket: Option<PathBuf>) -> ExitCode {
    let result = match action {
        OmarchyAction::Setup { dry_run } => setup(*dry_run, socket),
        OmarchyAction::Remove { dry_run } => remove(*dry_run, socket),
        OmarchyAction::Status { json } => status(*json),
        OmarchyAction::Keys => keys::print(),
    };
    result.unwrap_or_else(|err| {
        error!("{err}");
        ExitCode::FAILURE
    })
}

/// Where Omarchy and phux keep the files this command touches. Omarchy
/// spells its paths from `$HOME`, not the XDG variables, so these do too.
struct Paths {
    home: PathBuf,
    config: PathBuf,
    layer: PathBuf,
    hook: PathBuf,
    hypr: PathBuf,
}

impl Paths {
    fn resolve() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let data = phux_config::theme::themes_dir()
            .and_then(|themes| themes.parent().map(Path::to_path_buf))
            .ok_or("no data directory: set XDG_DATA_HOME or HOME")?;
        Ok(Self {
            config: config_loader::config_path(),
            layer: data.join("distros").join("omarchy").join("omarchy.toml"),
            hook: home.join(".config/omarchy/hooks/theme-set.d/phux"),
            hypr: home.join(".config/hypr"),
            home,
        })
    }

    /// Omarchy is installed for this user.
    fn omarchy_present(&self) -> bool {
        std::env::var_os("OMARCHY_PATH").is_some_and(|path| !path.is_empty())
            || self.home.join(".local/share/omarchy").is_dir()
    }

    fn hyprland(&self) -> Option<Hyprland> {
        Hyprland::detect(&self.hypr)
    }

    /// Whether the config selects this layer as its distro.
    fn layer_selected(&self) -> Result<bool, String> {
        let Ok(ours) = self.layer.canonicalize() else {
            return Ok(false);
        };
        Ok(current_distro(&self.config)?.is_some_and(|current| current == ours))
    }
}

/// The phux the desktop should run: this binary when its path needs no
/// quoting in Lua, hyprlang or a shell, else whatever `phux` is on `PATH`.
/// Hyprland's `PATH` need not include `~/.local/bin`, where the installer
/// puts phux.
fn phux_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.to_str().map(str::to_owned))
        .filter(|exe| {
            exe.chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._+-".contains(c))
        })
        .unwrap_or_else(|| "phux".to_owned())
}

/// One file change: new contents, or `None` to delete it.
struct Change {
    path: PathBuf,
    contents: Option<String>,
    what: &'static str,
}

impl Change {
    fn write(path: &Path, contents: String, what: &'static str) -> Self {
        Self {
            path: path.to_path_buf(),
            contents: Some(contents),
            what,
        }
    }

    fn delete(path: &Path, what: &'static str) -> Self {
        Self {
            path: path.to_path_buf(),
            contents: None,
            what,
        }
    }

    fn apply(&self) -> Result<(), String> {
        let fail = |err: std::io::Error| format!("{}: {err}", self.path.display());
        let Some(contents) = &self.contents else {
            return match std::fs::remove_file(&self.path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(fail(err)),
                _ => Ok(()),
            };
        };
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        std::fs::write(&self.path, contents).map_err(fail)
    }

    const fn verb(&self, dry_run: bool) -> &'static str {
        match (self.contents.is_some(), dry_run) {
            (true, true) => "would write",
            (true, false) => "wrote",
            (false, true) => "would delete",
            (false, false) => "deleted",
        }
    }
}

fn read(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// Omarchy runs hooks with `bash`; an executable one also runs by hand.
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// The base layer the Omarchy layer extends: the distro selected before
/// setup, carried over from our own layer when setup runs again.
fn base_distro(paths: &Paths) -> Result<Option<PathBuf>, String> {
    if paths.layer_selected()? {
        return Ok(layer_extends(&read(&paths.layer)?));
    }
    current_distro(&paths.config)
}

fn layer_extends(text: &str) -> Option<PathBuf> {
    let doc: toml::Table = toml::from_str(text).ok()?;
    let first = doc.get("extends")?.as_array()?.first()?.as_str()?;
    Some(PathBuf::from(first))
}

fn layer_text(base: Option<&Path>) -> String {
    base.map_or_else(
        || LAYER.to_owned(),
        |base| {
            let base = toml::Value::String(base.display().to_string());
            format!(
                "# The distro selected before `phux omarchy setup`.\nextends = [{base}]\n\n{LAYER}"
            )
        },
    )
}

fn setup(dry_run: bool, socket: Option<PathBuf>) -> Result<ExitCode, String> {
    let paths = Paths::resolve()?;
    if !paths.omarchy_present() {
        return Err(
            "Omarchy is not installed for this user (no $OMARCHY_PATH or ~/.local/share/omarchy)"
                .to_owned(),
        );
    }
    let phux = phux_command();
    let base = base_distro(&paths)?;
    let mut changes = vec![
        Change::write(
            &paths.layer,
            layer_text(base.as_deref()),
            "Omarchy key and theme layer",
        ),
        Change::write(
            &paths.hook,
            HOOK.replace("{PHUX}", &super::sh_quote(&phux)),
            "repaint phux when the Omarchy theme changes",
        ),
    ];
    let hyprland = paths.hyprland();
    if let Some(hypr) = &hyprland {
        let comment = hypr.comment();
        let bindings = blocks::upsert(&read(&hypr.bindings)?, comment, &hypr.bindings_block(&phux));
        let autostart = blocks::upsert(
            &read(&hypr.autostart)?,
            comment,
            &hypr.autostart_block(&phux),
        );
        changes.push(Change::write(
            &hypr.bindings,
            bindings,
            "Super+Ctrl+Return opens phux, Super+Ctrl+K lists its keys",
        ));
        changes.push(Change::write(
            &hypr.autostart,
            autostart,
            "start the phux server with the desktop session",
        ));
    }
    let theme = theme_choice()?;

    if dry_run {
        for change in &changes {
            outln!(
                "{} {}  ({})",
                change.verb(true),
                change.path.display(),
                change.what
            );
        }
        outln!(
            "would select the omarchy distro in {}",
            paths.config.display()
        );
        report_theme(theme, true);
        return Ok(ExitCode::SUCCESS);
    }
    // The config edits go first: they validate the whole config, and a
    // refusal there should leave the desktop untouched.
    let (layer, desktop) = changes.split_at(1);
    layer[0].apply()?;
    switch_distro(&paths.config, Some(&paths.layer))?;
    outln!("selected the omarchy distro in {}", paths.config.display());
    if theme == ThemeChoice::Unset {
        edit_theme_file(
            &paths.config,
            Edit::Set(toml::Value::String(THEME_FILE.to_owned())),
        )?;
    }
    report_theme(theme, false);
    for change in desktop {
        change.apply()?;
        outln!(
            "{} {}  ({})",
            change.verb(false),
            change.path.display(),
            change.what
        );
    }
    make_executable(&paths.hook)?;
    if hyprland.is_none() {
        outln!(
            "no Hyprland config in {}; skipped the desktop keys",
            paths.hypr.display()
        );
    }
    finish(socket, hyprland.is_some());
    outln!("Press Super+Ctrl+Return for phux, and Super+Ctrl+K for its keys.");
    Ok(ExitCode::SUCCESS)
}

fn remove(dry_run: bool, socket: Option<PathBuf>) -> Result<ExitCode, String> {
    let paths = Paths::resolve()?;
    let hyprland = paths.hyprland();
    let changes = removal_changes(&paths, hyprland.as_ref())?;
    let selected = paths.layer_selected()?;
    let theme = theme_choice()? == ThemeChoice::Omarchy;
    if changes.is_empty() && !selected && !theme {
        outln!("phux is not set up for Omarchy; nothing to remove");
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        if selected {
            outln!("would restore your previous distro selection");
        }
        if theme {
            outln!("would stop following the Omarchy theme");
        }
    } else {
        // Deselect before deleting the layer the config extends.
        restore_config(&paths, selected, theme)?;
    }
    for change in &changes {
        if !dry_run {
            change.apply()?;
        }
        outln!(
            "{} {}  ({})",
            change.verb(dry_run),
            change.path.display(),
            change.what
        );
    }
    if !dry_run {
        if let Some(dir) = paths.layer.parent() {
            // Only succeeds once empty, which is what we want.
            let _ = std::fs::remove_dir(dir);
        }
        finish(socket, hyprland.is_some());
    }
    Ok(ExitCode::SUCCESS)
}

/// The files `remove` edits or deletes: the marked blocks it finds and the
/// files setup owns outright.
fn removal_changes(paths: &Paths, hyprland: Option<&Hyprland>) -> Result<Vec<Change>, String> {
    let mut changes = Vec::new();
    if let Some(hypr) = hyprland {
        for (path, what) in [
            (&hypr.bindings, "phux desktop keys"),
            (&hypr.autostart, "phux server autostart"),
        ] {
            if let Some(contents) = blocks::strip(&read(path)?, hypr.comment()) {
                changes.push(Change::write(path, contents, what));
            }
        }
    }
    for (path, what) in [
        (&paths.hook, "theme-set hook"),
        (&paths.layer, "Omarchy layer"),
    ] {
        if path.exists() {
            changes.push(Change::delete(path, what));
        }
    }
    Ok(changes)
}

/// Put back the distro setup replaced and stop following Omarchy's theme,
/// when setup is what turned each on.
fn restore_config(paths: &Paths, selected: bool, theme: bool) -> Result<(), String> {
    if selected {
        let restore = layer_extends(&read(&paths.layer)?);
        switch_distro(&paths.config, restore.as_deref())?;
        match &restore {
            Some(base) => outln!("restored the {} distro", base.display()),
            None => outln!("deselected the omarchy distro"),
        }
    }
    if theme {
        edit_theme_file(&paths.config, Edit::Unset)?;
        outln!("phux no longer follows the Omarchy theme");
    }
    Ok(())
}

/// What selects phux's theme now, as far as setup cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeChoice {
    /// Nothing does: setup points `[theme] file` at Omarchy's theme.
    Unset,
    /// It already follows Omarchy's theme.
    Omarchy,
    /// The user picked another theme, which setup keeps.
    Other,
}

fn theme_choice() -> Result<ThemeChoice, String> {
    let cfg = config_loader::load().map_err(|err| err.to_string())?;
    let omarchy = phux_config::theme::expand_home(THEME_FILE);
    Ok(match cfg.theme.selection() {
        ThemeSelection::None => ThemeChoice::Unset,
        ThemeSelection::File(file) if phux_config::theme::expand_home(file) == omarchy => {
            ThemeChoice::Omarchy
        }
        ThemeSelection::File(_) | ThemeSelection::Name(_) => ThemeChoice::Other,
    })
}

fn report_theme(choice: ThemeChoice, dry_run: bool) {
    match (choice, dry_run) {
        (ThemeChoice::Unset, true) => outln!("would follow the Omarchy theme ({THEME_FILE})"),
        (ThemeChoice::Unset | ThemeChoice::Omarchy, _) => {
            outln!("phux follows the Omarchy theme ({THEME_FILE})");
        }
        (ThemeChoice::Other, _) => outln!(
            "kept the theme your config selects; `phux theme set --file {THEME_FILE}` follows Omarchy's"
        ),
    }
}

fn edit_theme_file(config: &Path, edit: Edit) -> Result<(), String> {
    phux_config::settings::write_edit(config, &format!("theme.{FILE_KEY}"), edit)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Repaint attached clients and reload Hyprland's binds. Neither is an
/// error when absent: the files are in place for the next start.
fn finish(socket: Option<PathBuf>, hyprland: bool) {
    let socket_path = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    if let Ok(rt) = crate::commands::cli_runtime()
        && rt
            .block_on(super::config::ring_config_reload(&socket_path))
            .is_ok()
    {
        outln!("reloaded attached phux clients");
    }
    let in_hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    if hyprland
        && in_hyprland
        && std::process::Command::new("hyprctl")
            .arg("reload")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    {
        outln!("reloaded Hyprland");
    }
}

fn status(json: bool) -> Result<ExitCode, String> {
    let paths = Paths::resolve()?;
    let hyprland = paths.hyprland();
    let has_block = |pick: fn(&Hyprland) -> &PathBuf| {
        hyprland.as_ref().is_some_and(|hypr| {
            read(pick(hypr)).is_ok_and(|text| blocks::contains(&text, hypr.comment()))
        })
    };
    let checks = [
        ("omarchy", paths.omarchy_present()),
        ("distro", paths.layer_selected()?),
        ("theme", theme_choice()? == ThemeChoice::Omarchy),
        ("theme_hook", paths.hook.is_file()),
        ("desktop_keys", has_block(|hypr| &hypr.bindings)),
        ("autostart", has_block(|hypr| &hypr.autostart)),
    ];
    if json {
        let checks: serde_json::Map<_, _> = checks
            .iter()
            .map(|(name, ok)| ((*name).to_owned(), serde_json::Value::Bool(*ok)))
            .collect();
        return Ok(crate::output::json(
            &serde_json::json!({ "schema_version": 1, "checks": checks }),
        ));
    }
    for (name, ok) in checks {
        outln!("{} {name}", if ok { "✓" } else { "✗" });
    }
    if checks.iter().any(|(_, ok)| !ok) {
        outln!("`phux omarchy setup` puts the missing pieces in place.");
    }
    Ok(ExitCode::SUCCESS)
}
