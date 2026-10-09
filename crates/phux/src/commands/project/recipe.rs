//! The `.phux/project.toml` recipe (ADR-0152): parse, validate, and compile
//! into the workspace archive `workspace restore` already knows how to
//! replay, so a recipe adds no second session-building engine.
//!
//! Everything is checked before any process starts: unknown keys, ids,
//! split targets, the focus reference, working directories, and the
//! environment. Commands are argument vectors and never pass through a
//! shell, except for the fixed `sh -c` wrapper that drops a non-`exec`
//! pane back to a prompt when its command exits.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::commands::workspace::archive::model::{
    ARCHIVE_SCHEMA_VERSION, WorkspaceArchive, WorkspaceLayoutNode, WorkspacePane, WorkspaceSession,
    WorkspaceSplitDir, WorkspaceWindow,
};

/// The largest recipe file read, in bytes.
pub(crate) const MAX_RECIPE_BYTES: u64 = 64 * 1024;
/// The most panes one recipe may start.
const MAX_PANES: usize = 64;
/// `$0` of the fallback wrapper, so `ps` shows where the shell came from.
const WRAPPER_ARGV0: &str = "phux-project";
/// Run the command, then replace the wrapper with the user's shell.
const WRAPPER_SCRIPT: &str = "\"$@\"; exec \"${SHELL:-/bin/sh}\"";

/// The recipe document. Unknown keys are refused, so a typo cannot silently
/// drop a pane.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recipe {
    /// Environment for every pane; windows and panes layer over it.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// `WINDOW.PANE` ids of the pane to focus. Default: the first pane.
    focus: Option<String>,
    /// The session's windows, in order. Empty means one shell pane.
    #[serde(default)]
    windows: Vec<RecipeWindow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeWindow {
    /// Recipe-local reference for `focus`; never displayed.
    id: Option<String>,
    /// The window's displayed name.
    name: Option<String>,
    /// Working directory for this window's panes, relative to the root.
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    panes: Vec<RecipePane>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipePane {
    /// Recipe-local reference for `focus` and `split.target`.
    id: Option<String>,
    /// Argument vector; absent starts the default shell.
    command: Option<Vec<String>>,
    /// Make `command` the pane's top-level process: the pane closes when
    /// it exits instead of falling back to a shell.
    #[serde(default)]
    exec: bool,
    /// Working directory, relative to the root; overrides the window's.
    cwd: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Where this pane goes. Required on every pane but a window's first.
    split: Option<RecipeSplit>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeSplit {
    /// The id of an earlier pane in the same window.
    target: String,
    direction: SplitDirection,
    /// Fraction of the split the target keeps, strictly between 0 and 1.
    ratio: Option<f32>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SplitDirection {
    Right,
    Down,
}

/// Parse `text` without resolving anything against the filesystem.
pub(crate) fn parse(text: &str) -> Result<Recipe, String> {
    toml::from_str(text).map_err(|err| format!("invalid project recipe: {err}"))
}

/// The recipe a new checkout starts from (`phux project init`).
pub(crate) const STARTER: &str = "\
# phux project recipe (ADR-0152). `phux project open` builds this session
# the first time; reopening a live session never reruns it. Commands run
# only after you approve these exact bytes (`phux project trust`).

[[windows]]
name = \"code\"
panes = [
  { id = \"editor\", command = [\"sh\", \"-c\", \"exec ${EDITOR:-vi} .\"] },
  { split = { target = \"editor\", direction = \"right\", ratio = 0.6 } },
]

[[windows]]
name = \"shell\"
";

/// Compile `recipe` into a one-session archive rooted at `root`, named
/// `session`. `None` compiles the no-recipe default: one shell at the root.
pub(crate) fn compile(
    recipe: Option<&Recipe>,
    root: &Path,
    session: &str,
) -> Result<WorkspaceArchive, String> {
    let empty = Recipe {
        env: BTreeMap::new(),
        focus: None,
        windows: Vec::new(),
    };
    let recipe = recipe.unwrap_or(&empty);
    check_env(&recipe.env, "the recipe")?;
    let focus = parse_focus(recipe.focus.as_deref())?;

    let mut windows = Vec::new();
    let mut window_ids = HashSet::new();
    let mut panes_total = 0usize;
    let mut focused = false;
    for (index, window) in recipe.windows.iter().enumerate() {
        let label = window_label(window, index);
        if let Some(id) = &window.id {
            check_id(id, &label)?;
            if !window_ids.insert(id.as_str()) {
                return Err(format!("{label}: duplicate window id {id:?}"));
            }
        }
        check_env(&window.env, &label)?;
        let wants_focus = focus
            .as_ref()
            .is_some_and(|(win, _)| window.id.as_deref() == Some(win.as_str()));
        let focus_pane = wants_focus
            .then(|| focus.as_ref().map(|(_, pane)| pane.as_str()))
            .flatten();
        let compiled = compile_window(recipe, window, &label, root, focus_pane)?;
        panes_total += compiled.panes.len();
        focused |= compiled.active;
        windows.push(compiled);
    }
    if let Some((win, pane)) = &focus
        && !focused
    {
        return Err(format!(
            "focus {win}.{pane:?} names no window and pane with those ids"
        ));
    }
    if windows.is_empty() {
        windows.push(default_window(recipe, root)?);
        panes_total = 1;
    }
    if panes_total > MAX_PANES {
        return Err(format!(
            "the recipe starts {panes_total} panes; the limit is {MAX_PANES}"
        ));
    }
    if !windows.iter().any(|window| window.active) {
        windows[0].active = true;
        if let Some(pane) = windows[0].panes.first_mut() {
            pane.active = true;
        }
    }
    Ok(WorkspaceArchive {
        schema_version: ARCHIVE_SCHEMA_VERSION,
        sessions: vec![WorkspaceSession {
            name: session.to_owned(),
            active: true,
            cwd: None,
            host: None,
            project: None,
            command: None,
            windows,
        }],
    })
}

fn window_label(window: &RecipeWindow, index: usize) -> String {
    match (&window.id, &window.name) {
        (Some(id), _) => format!("window {id:?}"),
        (None, Some(name)) => format!("window {name:?}"),
        (None, None) => format!("window {}", index + 1),
    }
}

/// One shell pane at the root, carrying the recipe-wide environment.
fn default_window(recipe: &Recipe, root: &Path) -> Result<WorkspaceWindow, String> {
    Ok(WorkspaceWindow {
        name: "shell".to_owned(),
        active: true,
        layout: None,
        panes: vec![WorkspacePane {
            active: true,
            title: None,
            cwd: Some(resolve_cwd(root, None, "the default pane")?),
            command: None,
            agent_session: None,
            env: recipe.env.clone(),
            cols: 0,
            rows: 0,
        }],
    })
}

fn compile_window(
    recipe: &Recipe,
    window: &RecipeWindow,
    label: &str,
    root: &Path,
    focus_pane: Option<&str>,
) -> Result<WorkspaceWindow, String> {
    if window.panes.is_empty() {
        // A window with no panes is one shell, the same as `panes = [{}]`.
        let pane = RecipePane {
            id: None,
            command: None,
            exec: false,
            cwd: None,
            env: BTreeMap::new(),
            split: None,
        };
        return compile_panes(
            recipe,
            window,
            std::slice::from_ref(&pane),
            label,
            root,
            focus_pane,
        );
    }
    compile_panes(recipe, window, &window.panes, label, root, focus_pane)
}

fn compile_panes(
    recipe: &Recipe,
    window: &RecipeWindow,
    panes: &[RecipePane],
    label: &str,
    root: &Path,
    focus_pane: Option<&str>,
) -> Result<WorkspaceWindow, String> {
    let mut ids: BTreeMap<&str, usize> = BTreeMap::new();
    let mut layout = WorkspaceLayoutNode::Pane { pane: 0 };
    let mut compiled = Vec::with_capacity(panes.len());
    for (index, pane) in panes.iter().enumerate() {
        let pane_label = pane.id.as_ref().map_or_else(
            || format!("{label} pane {}", index + 1),
            |id| format!("{label} pane {id:?}"),
        );
        check_env(&pane.env, &pane_label)?;
        place_pane(&mut layout, &ids, pane, index, &pane_label)?;
        if let Some(id) = &pane.id {
            check_id(id, &pane_label)?;
            if ids.insert(id.as_str(), index).is_some() {
                return Err(format!("{label}: duplicate pane id {id:?}"));
            }
        }
        let mut env = recipe.env.clone();
        env.extend(window.env.clone());
        env.extend(pane.env.clone());
        compiled.push(WorkspacePane {
            active: false,
            title: None,
            cwd: Some(resolve_cwd(
                root,
                pane.cwd.as_deref().or(window.cwd.as_deref()),
                &pane_label,
            )?),
            command: pane_command(pane, &pane_label)?,
            agent_session: None,
            env,
            cols: 0,
            rows: 0,
        });
    }
    let active = match focus_pane {
        Some(id) => {
            let Some(&index) = ids.get(id) else {
                return Err(format!(
                    "focus names pane {id:?}, which {label} does not have"
                ));
            };
            compiled[index].active = true;
            true
        }
        None => false,
    };
    let name = window
        .name
        .clone()
        .or_else(|| window.id.clone())
        .unwrap_or_else(|| default_window_name(&panes[0]));
    Ok(WorkspaceWindow {
        name,
        active,
        layout: Some(layout),
        panes: compiled,
    })
}

/// Splice pane `index` into `layout` beside its split target. The first pane
/// is the layout's seed and must not split; every later one must.
fn place_pane(
    layout: &mut WorkspaceLayoutNode,
    ids: &BTreeMap<&str, usize>,
    pane: &RecipePane,
    index: usize,
    label: &str,
) -> Result<(), String> {
    match (&pane.split, index) {
        (None, 0) => Ok(()),
        (Some(_), 0) => Err(format!(
            "{label}: the first pane in a window has nothing to split; remove `split`"
        )),
        (None, _) => Err(format!(
            "{label}: every pane after the first needs `split = {{ target, direction }}`"
        )),
        (Some(split), _) => {
            let Some(&target) = ids.get(split.target.as_str()) else {
                return Err(format!(
                    "{label}: split target {:?} is not an earlier pane in this window",
                    split.target
                ));
            };
            let ratio = split.ratio.unwrap_or(0.5);
            if !(ratio > 0.0 && ratio < 1.0) {
                return Err(format!(
                    "{label}: split ratio {ratio} must be strictly between 0 and 1"
                ));
            }
            let dir = match split.direction {
                SplitDirection::Right => WorkspaceSplitDir::Horizontal,
                SplitDirection::Down => WorkspaceSplitDir::Vertical,
            };
            if splice(layout, target, index, dir, ratio) {
                Ok(())
            } else {
                Err(format!("{label}: split target was not placed"))
            }
        }
    }
}

fn splice(
    node: &mut WorkspaceLayoutNode,
    target: usize,
    new: usize,
    dir: WorkspaceSplitDir,
    ratio: f32,
) -> bool {
    match node {
        WorkspaceLayoutNode::Pane { pane } if *pane == target => {
            *node = WorkspaceLayoutNode::Split {
                dir,
                ratio,
                left: Box::new(WorkspaceLayoutNode::Pane { pane: target }),
                right: Box::new(WorkspaceLayoutNode::Pane { pane: new }),
            };
            true
        }
        WorkspaceLayoutNode::Pane { .. } => false,
        WorkspaceLayoutNode::Split { left, right, .. } => {
            splice(left, target, new, dir, ratio) || splice(right, target, new, dir, ratio)
        }
    }
}

/// The argv a pane spawns: absent for the default shell, the command itself
/// under `exec`, otherwise the command followed by the user's shell.
fn pane_command(pane: &RecipePane, label: &str) -> Result<Option<Vec<String>>, String> {
    let Some(argv) = &pane.command else {
        if pane.exec {
            return Err(format!("{label}: `exec = true` needs a `command`"));
        }
        return Ok(None);
    };
    if argv.first().is_none_or(String::is_empty) {
        return Err(format!("{label}: `command` must name a program"));
    }
    if argv.iter().any(|arg| arg.contains('\0')) {
        return Err(format!("{label}: `command` contains a NUL byte"));
    }
    if pane.exec {
        return Ok(Some(argv.clone()));
    }
    let mut wrapped = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        WRAPPER_SCRIPT.to_owned(),
        WRAPPER_ARGV0.to_owned(),
    ];
    wrapped.extend(argv.iter().cloned());
    Ok(Some(wrapped))
}

fn default_window_name(first: &RecipePane) -> String {
    first
        .command
        .as_ref()
        .and_then(|argv| argv.first())
        .and_then(|program| Path::new(program).file_name())
        .map_or_else(
            || "shell".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
}

/// An absolute working directory that exists, from a root-relative (or
/// absolute) `cwd`.
fn resolve_cwd(root: &Path, cwd: Option<&str>, label: &str) -> Result<String, String> {
    let path = cwd.map_or_else(|| root.to_path_buf(), |cwd| root.join(cwd));
    let resolved: PathBuf = path
        .canonicalize()
        .map_err(|err| format!("{label}: working directory {}: {err}", path.display()))?;
    if !resolved.is_dir() {
        return Err(format!(
            "{label}: working directory {} is not a directory",
            resolved.display()
        ));
    }
    Ok(resolved.display().to_string())
}

/// Ids are recipe-local references: ASCII letters, digits, `-`, `_`.
fn check_id(id: &str, label: &str) -> Result<(), String> {
    if !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Ok(());
    }
    Err(format!(
        "{label}: id {id:?} must be non-empty ASCII letters, digits, `-`, or `_`"
    ))
}

/// `PHUX_*` belongs to phux (a recipe must not repoint `PHUX_SOCKET`).
fn check_env(env: &BTreeMap<String, String>, label: &str) -> Result<(), String> {
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0') {
            return Err(format!("{label}: invalid environment variable {key:?}"));
        }
        if key.starts_with("PHUX_") {
            return Err(format!(
                "{label}: {key} is reserved for phux and cannot be set by a recipe"
            ));
        }
    }
    Ok(())
}

fn parse_focus(focus: Option<&str>) -> Result<Option<(String, String)>, String> {
    let Some(focus) = focus else {
        return Ok(None);
    };
    let Some((window, pane)) = focus.split_once('.') else {
        return Err(format!("focus {focus:?} must be WINDOW.PANE ids"));
    };
    check_id(window, "focus")?;
    check_id(pane, "focus")?;
    Ok(Some((window.to_owned(), pane.to_owned())))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("web")).expect("mkdir");
        dir
    }

    fn compile_text(text: &str, root: &Path) -> Result<WorkspaceArchive, String> {
        compile(Some(&parse(text)?), root, "demo")
    }

    #[test]
    fn the_starter_recipe_compiles() {
        let dir = root();
        let archive = compile_text(STARTER, dir.path()).expect("starter compiles");
        let windows = &archive.sessions[0].windows;
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].panes.len(), 2);
        assert!(windows[0].active && windows[0].panes[0].active);
    }

    #[test]
    fn no_recipe_is_one_shell_at_the_root() {
        let dir = root();
        let archive = compile(None, dir.path(), "demo").expect("default");
        let session = &archive.sessions[0];
        assert_eq!(session.name, "demo");
        assert_eq!(session.windows.len(), 1);
        let pane = &session.windows[0].panes[0];
        assert!(pane.active && pane.command.is_none());
        let canonical = dir.path().canonicalize().expect("canonical");
        assert_eq!(pane.cwd.as_deref(), Some(canonical.to_str().expect("utf8")));
    }

    #[test]
    fn splits_build_the_tree_and_focus_and_env_layer() {
        let dir = root();
        let archive = compile_text(
            r#"
            env = { A = "recipe", B = "recipe" }
            focus = "main.agent"
            [[windows]]
            id = "main"
            env = { B = "window" }
            panes = [
              { id = "editor", command = ["nvim"], exec = true },
              { id = "agent", command = ["claude"], cwd = "web", env = { C = "pane" },
                split = { target = "editor", direction = "right", ratio = 0.6 } },
              { split = { target = "agent", direction = "down" } },
            ]
            "#,
            dir.path(),
        )
        .expect("compiles");
        let window = &archive.sessions[0].windows[0];
        assert_eq!(window.name, "main");
        assert!(window.active);
        assert!(!window.panes[0].active && window.panes[1].active);
        assert_eq!(
            window.panes[0].command.as_deref(),
            Some(&["nvim".to_owned()][..])
        );
        let agent = window.panes[1].command.as_ref().expect("command");
        assert_eq!(
            &agent[..4],
            ["/bin/sh", "-c", WRAPPER_SCRIPT, WRAPPER_ARGV0]
        );
        assert_eq!(agent[4], "claude");
        assert!(
            window.panes[1]
                .cwd
                .as_deref()
                .is_some_and(|cwd| cwd.ends_with("/web"))
        );
        assert_eq!(window.panes[1].env["A"], "recipe");
        assert_eq!(window.panes[1].env["B"], "window");
        assert_eq!(window.panes[1].env["C"], "pane");
        let Some(WorkspaceLayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        }) = &window.layout
        else {
            panic!("expected a split root, got {:?}", window.layout);
        };
        assert_eq!(*dir, WorkspaceSplitDir::Horizontal);
        assert!((ratio - 0.6).abs() < f32::EPSILON);
        assert!(matches!(**left, WorkspaceLayoutNode::Pane { pane: 0 }));
        assert!(matches!(
            **right,
            WorkspaceLayoutNode::Split {
                dir: WorkspaceSplitDir::Vertical,
                ..
            }
        ));
    }

    #[test]
    fn invalid_recipes_are_refused_before_anything_runs() {
        let dir = root();
        for (text, needle) in [
            ("bogus = 1", "unknown field"),
            (
                "[[windows]]\npanes = [{ split = { target = \"x\", direction = \"right\" } }]",
                "first pane",
            ),
            ("[[windows]]\npanes = [{}, {}]", "needs `split"),
            (
                "[[windows]]\npanes = [{}, { split = { target = \"nope\", direction = \"down\" } }]",
                "not an earlier pane",
            ),
            (
                "[[windows]]\npanes = [{ id = \"a\" }, { split = { target = \"a\", direction = \"down\", ratio = 1.0 } }]",
                "strictly between",
            ),
            ("env = { PHUX_SOCKET = \"/x\" }", "reserved"),
            ("focus = \"nowhere.pane\"", "names no window"),
            ("[[windows]]\ncwd = \"missing\"", "working directory"),
            ("[[windows]]\npanes = [{ command = [] }]", "name a program"),
            (
                "[[windows]]\npanes = [{ exec = true }]",
                "needs a `command`",
            ),
            ("[[windows]]\nid = \"bad id\"", "must be non-empty"),
        ] {
            let err = compile_text(text, dir.path()).expect_err(text);
            assert!(err.contains(needle), "{text:?}: {err}");
        }
    }
}
