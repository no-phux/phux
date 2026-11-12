//! `phux migrate`: read the user's tmux, zellij, Ghostty, herdr and Rex configs,
//! decide every line, and hand the result to their coding agent as a strict
//! prompt, in a pane beside them. phux translates; the agent writes, checks,
//! reloads and reports. Nothing here edits the phux config itself.

mod prompt;
mod sources;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::theme::ThemeSelection;
use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{FrameKind, SessionLifetime, SpawnResult};
use phux_server::runtime::default_socket_path;
use serde_json::{Value, json};

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerSpec;
use crate::commands::{MigrateAgent, MigrateArgs, SpawnSplit, new, sh_quote, spawn};
use crate::exit_codes::EXIT_USAGE;

/// Version of the `phux migrate --json` document.
const SCHEMA_VERSION: u8 = 1;

#[allow(
    clippy::too_many_lines,
    reason = "one linear verb: decide the sources, render the prompt, launch the agent"
)]
pub(crate) fn run(args: MigrateArgs, socket: Option<PathBuf>) -> ExitCode {
    let MigrateArgs {
        agent,
        print,
        source,
        json,
        prefix,
        advanced,
    } = args;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let config = phux_config::loader::config_path();
    let theme = selected_theme(&config);
    let setup = sources::Setup { prefix, advanced };
    let ask = !print && !json;
    let (setup, found) = match decide(setup, &home, xdg.as_deref(), &source, theme.as_deref(), ask)
    {
        Ok(decided) => decided,
        Err(err) => return json_err::emit(json, &err, EXIT_USAGE),
    };
    let doc = |agent, launched: Option<&Value>, bytes| {
        document(
            &found,
            agent,
            &config,
            theme.as_deref(),
            launched,
            bytes,
            &setup,
        )
    };
    let installed = installed_agents();
    if found.is_empty() {
        if json {
            return crate::output::json(&doc(None, None, 0));
        }
        let message = "Nothing to migrate: no tmux, zellij, Ghostty, herdr or Rex config found.";
        if print {
            info!("{message}");
        } else {
            outln!("{message}");
        }
        return ExitCode::SUCCESS;
    }
    let phux = std::env::current_exe().map_or_else(
        |_| "phux".to_owned(),
        |exe| sh_quote(&exe.to_string_lossy()),
    );
    let reference = prompt::reference();
    let prefix = setup.chosen_prefix();
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let text = prompt::render(
        &found,
        &prompt::Context {
            home: &home,
            phux: &phux,
            config: &config,
            date: &date,
            prefix: &prefix,
            reference: &reference,
        },
    );
    if print {
        if json {
            let picked = agent.or(match installed.as_slice() {
                [(only, _)] => Some(*only),
                _ => None,
            });
            let mut doc = doc(picked, None, text.len());
            doc["prompt"] = Value::String(text);
            return crate::output::json(&doc);
        }
        out!("{text}");
        return ExitCode::SUCCESS;
    }
    let (agent, program) = match choose_agent(agent, &installed, json) {
        Ok(choice) => choice,
        Err(code) => return code,
    };
    let dir = config
        .parent()
        .map_or_else(|| home.clone(), Path::to_path_buf);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        return json_err::emit(
            json,
            &CliError::new(
                codes::IO,
                format!("cannot create {}: {err}", dir.display()),
                "check the permissions of your config directory",
            ),
            1,
        );
    }
    let command = agent_argv(agent, &program, text.clone());
    let launched = match launch(command, &dir, socket, json) {
        Ok(launched) => launched,
        Err(code) => return code,
    };
    if json {
        return crate::output::json(&doc(Some(agent), launched.as_ref(), text.len()));
    }
    ExitCode::SUCCESS
}

/// The phux theme the config selects (`[theme] file`, else `name`, as
/// `phux theme set` writes them), or `None`. An unreadable config selects
/// none.
fn selected_theme(config: &Path) -> Option<String> {
    let cfg = phux_config::loader::load_from(config).ok()?;
    match cfg.theme.selection() {
        ThemeSelection::Name(theme) | ThemeSelection::File(theme) => Some(theme.to_owned()),
        ThemeSelection::None => None,
    }
}

/// The choices from the flags, the questions they leave open (asked only
/// when `ask` and on a terminal), and the sources decided with them.
fn decide(
    mut setup: sources::Setup,
    home: &Path,
    xdg: Option<&Path>,
    only: &[crate::commands::MigrateSource],
    theme: Option<&str>,
    ask: bool,
) -> Result<(sources::Setup, Vec<sources::Source>), CliError> {
    if let Some(chord) = &setup.prefix
        && let Err(err) = phux_config::keybind::parse_chord(chord)
    {
        return Err(CliError::new(
            codes::USAGE_ERROR,
            format!("--prefix {chord}: {err}"),
            "spell it as a phux key, such as C-b",
        ));
    }
    let found = sources::discover(home, xdg, only, theme, &setup);
    // Both questions are about tmux.
    let Some(tmux) = found.iter().find_map(|source| source.prefix.clone()) else {
        return Ok((setup, found));
    };
    let flags = (setup.prefix.clone(), setup.advanced);
    if ask && std::io::stdin().is_terminal() {
        ask_setup(&mut setup, tmux);
    } else if setup.advanced && setup.prefix.is_none() {
        setup.prefix = Some(tmux);
    }
    if (setup.prefix.clone(), setup.advanced) == flags {
        return Ok((setup, found));
    }
    let found = sources::discover(home, xdg, only, theme, &setup);
    Ok((setup, found))
}

/// The `--json` document. Stable: the Rune onboarding reads it.
fn document(
    found: &[sources::Source],
    agent: Option<MigrateAgent>,
    config: &Path,
    theme: Option<&str>,
    launched: Option<&Value>,
    prompt_bytes: usize,
    setup: &sources::Setup,
) -> Value {
    let sources: Vec<Value> = found
        .iter()
        .map(|source| {
            let (bindings, unbindings, settings) = source.found();
            json!({
                "name": source_id(source.kind),
                "files": source.files,
                "found": { "bindings": bindings, "unbindings": unbindings, "settings": settings },
            })
        })
        .collect();
    json!({
        "schema_version": SCHEMA_VERSION,
        "sources": sources,
        "agent": agent.map(agent_id),
        "config_path": config,
        "theme": theme,
        "launched": launched,
        "prompt_bytes": prompt_bytes,
        "prefix": prefix_json(found, setup),
        "mode": if setup.advanced { "advanced" } else { "beginner" },
    })
}

/// The prefix choice: phux's own, tmux's (`null` without tmux) and the
/// one the migrated config ends up with.
fn prefix_json(found: &[sources::Source], setup: &sources::Setup) -> Value {
    json!({
        "chosen": setup.chosen_prefix(),
        "tmux": found.iter().find_map(|source| source.prefix.as_deref()),
        "phux": sources::shipped_bindings().prefix,
    })
}

/// Ask on the terminal what the flags left open: the setup mode, then
/// whether to keep phux's prefix or take tmux's. Enter takes beginner, and
/// the prefix the mode prefers.
fn ask_setup(setup: &mut sources::Setup, tmux: String) {
    if !setup.advanced {
        setup.advanced = pick(
            "How closely should phux follow your tmux setup?",
            &[
                "beginner: keep phux's defaults where they already do what you wanted".into(),
                "advanced: mirror tmux as closely as phux allows".into(),
            ],
            0,
        ) == 1;
    }
    let phux = sources::shipped_bindings().prefix;
    if setup.prefix.is_some()
        || phux_config::keybind::parse_chord(&tmux).ok()
            == phux_config::keybind::parse_chord(&phux).ok()
    {
        return;
    }
    let choice = pick(
        &format!("tmux's prefix is {tmux}; phux's is {phux}. Which prefix should phux use?"),
        &[format!("keep phux's {phux}"), format!("use tmux's {tmux}")],
        usize::from(setup.advanced),
    );
    if choice == 1 {
        setup.prefix = Some(tmux);
    }
}

/// Ask one numbered question on the terminal; Enter, or an answer that is
/// not a choice, takes `default`.
fn pick(question: &str, choices: &[String], default: usize) -> usize {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "{question}");
    for (n, choice) in choices.iter().enumerate() {
        let _ = writeln!(stderr, "  {}) {choice}", n + 1);
    }
    let _ = write!(stderr, "Choose [{}]: ", default + 1);
    let _ = stderr.flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    answer
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=choices.len()).contains(n))
        .map_or(default, |n| n - 1)
}

const fn source_id(kind: crate::commands::MigrateSource) -> &'static str {
    use crate::commands::MigrateSource;
    match kind {
        MigrateSource::Tmux => "tmux",
        MigrateSource::Zellij => "zellij",
        MigrateSource::Ghostty => "ghostty",
        MigrateSource::Herdr => "herdr",
        MigrateSource::Rex => "rex",
    }
}

const fn agent_id(agent: MigrateAgent) -> &'static str {
    match agent {
        MigrateAgent::Claude => "claude",
        MigrateAgent::Codex => "codex",
        MigrateAgent::Opencode => "opencode",
    }
}

/// Each supported agent found on this machine, with its executable.
fn installed_agents() -> Vec<(MigrateAgent, PathBuf)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let find = |name: &str| {
        phux_plugin::executable_path(name).or_else(|| user_executable(name, home.as_deref()))
    };
    [
        (MigrateAgent::Claude, find("claude")),
        (MigrateAgent::Codex, find("codex")),
        (
            MigrateAgent::Opencode,
            phux_plugin::opencode_executable_path(),
        ),
    ]
    .into_iter()
    .filter_map(|(agent, path)| Some((agent, path?)))
    .collect()
}

/// An agent installed where its installer puts it but off `PATH`: the case
/// for Rune and other GUI launches, whose `PATH` is launchd's minimal one.
fn user_executable(name: &str, home: Option<&Path>) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = home
        .map(|home| vec![home.join(".local/bin"), home.join(".claude/local")])
        .unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    let path = std::env::join_paths(dirs).ok()?;
    phux_plugin::executable_on_path(name, &path)
}

/// How each agent takes its first message on the command line.
fn agent_argv(agent: MigrateAgent, program: &Path, prompt: String) -> Vec<String> {
    let flag = match agent {
        MigrateAgent::Claude | MigrateAgent::Codex => "--",
        MigrateAgent::Opencode => "--prompt",
    };
    vec![
        program.to_string_lossy().into_owned(),
        flag.to_owned(),
        prompt,
    ]
}

fn choose_agent(
    wanted: Option<MigrateAgent>,
    installed: &[(MigrateAgent, PathBuf)],
    json: bool,
) -> Result<(MigrateAgent, PathBuf), ExitCode> {
    let names = || {
        installed
            .iter()
            .map(|(agent, _)| agent_id(*agent))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let refuse = |message: String, remedy: &str| {
        json_err::emit(
            json,
            &CliError::new(codes::USAGE_ERROR, message, remedy),
            EXIT_USAGE,
        )
    };
    if let Some(wanted) = wanted {
        return installed
            .iter()
            .find(|(agent, _)| *agent == wanted)
            .cloned()
            .ok_or_else(|| {
                refuse(
                    format!("{} is not installed", agent_id(wanted)),
                    "install it, pick another with --agent, or run `phux migrate --print` and \
                     paste the prompt into your agent",
                )
            });
    }
    match installed {
        [] => Err(refuse(
            "no coding agent found: phux migrate starts claude, codex or opencode".into(),
            "install one, or run `phux migrate --print` and paste the prompt into your agent",
        )),
        [only] => Ok(only.clone()),
        _ if json || !std::io::stdin().is_terminal() => Err(refuse(
            format!("several agents are installed: {}", names()),
            "pick one with --agent",
        )),
        _ => {
            ask(installed).ok_or_else(|| refuse("no agent chosen".into(), "pick one with --agent"))
        }
    }
}

/// Ask on the terminal which agent to start; Enter takes the first.
fn ask(installed: &[(MigrateAgent, PathBuf)]) -> Option<(MigrateAgent, PathBuf)> {
    let mut stderr = std::io::stderr();
    let _ = writeln!(stderr, "Which agent should migrate your config?");
    for (n, (agent, _)) in installed.iter().enumerate() {
        let _ = writeln!(stderr, "  {}) {}", n + 1, agent_id(*agent));
    }
    let _ = write!(stderr, "Choose [1]: ");
    let _ = stderr.flush();
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer).ok()?;
    let answer = answer.trim();
    if answer.is_empty() {
        return installed.first().cloned();
    }
    installed
        .iter()
        .enumerate()
        .find(|(n, (agent, _))| answer == (n + 1).to_string() || answer == agent_id(*agent))
        .map(|(_, choice)| choice.clone())
}

/// The pane this process runs in, when it belongs to the server `socket`
/// resolves to: the agent then opens in a split beside it.
fn own_pane(socket: Option<&Path>) -> Option<u32> {
    let id = std::env::var("PHUX_TERMINAL_ID").ok()?.parse().ok()?;
    let inherited = PathBuf::from(std::env::var_os("PHUX_SOCKET")?);
    let target = socket.map_or_else(default_socket_path, Path::to_path_buf);
    (inherited == target).then_some(id)
}

/// Start `argv` in a split to the right of this pane, or in a new session
/// named `migrate` (attached on a terminal). Returns the `launched` JSON.
fn launch(
    argv: Vec<String>,
    dir: &Path,
    socket: Option<PathBuf>,
    json: bool,
) -> Result<Option<Value>, ExitCode> {
    let cwd = dir.to_string_lossy().into_owned();
    if let Some(pane) = own_pane(socket.as_deref()) {
        let socket_path = socket.unwrap_or_else(default_socket_path);
        let frame = FrameKind::SpawnResource {
            request_id: 1,
            group: GroupId::new(1),
            command: Some(argv),
            cwd: Some(cwd),
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        };
        let result = spawn::dispatch_spawn_placed(
            &socket_path,
            frame,
            1,
            "migrate",
            &format!("@{pane}"),
            SpawnSplit::Horizontal,
            0.5,
            None,
            None,
            json,
        )?;
        let SpawnResult::Ok(id) = result else {
            return Err(match result {
                SpawnResult::Err(err) => spawn::report_spawn_error(json, &err),
                _ => ExitCode::FAILURE,
            });
        };
        let pane = phux_client::selector::format_terminal_id(&id);
        if !json {
            outln!("Your agent is migrating your config in {pane}, to the right.");
        }
        return Ok(Some(json!({ "pane": pane })));
    }

    let spec = ServerSpec::local(socket);
    let (rt, target) = spec.prepare("migrate", json)?;
    new::ensure_local_unseeded_server(&target, true);
    let name = new::unique_session_name(&new::existing_session_names(&rt, &target), "migrate");
    if !json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        drop(rt);
        let code = new::run_new(
            None,
            Some(name),
            Some(dir.to_path_buf()),
            ServerSpec::local(target.socket_path().map(Path::to_path_buf)),
            new::NewMode::default(),
            argv,
            Vec::new(),
        );
        // Attached: the session's end is the verb's end, whatever its code.
        return Err(code);
    }
    let id = rt.block_on(new::create_session_via_metadata(
        &target,
        &name,
        Some(argv),
        Some(cwd),
        BTreeMap::new(),
        None,
        json,
        None,
        SessionLifetime::Persistent,
        &[],
    ))?;
    if !json {
        outln!("Your agent is migrating your config in session {name}. Watch it with:");
        outln!("  phux attach {name}");
    }
    Ok(Some(json!({ "session": name, "pane": format!("@{id}") })))
}
