//! Strict MCP wrappers over canonical `phux` CLI orchestration verbs.

#![allow(
    clippy::similar_names,
    reason = "argv and parsed args are deliberately adjacent in thin CLI wrappers"
)]

use serde_json::{Map, Value, json};

use crate::cli_adapter::{
    CliAdapter, DEFAULT_CALL_TIMEOUT, bounded_string, bounded_strings, enum_string, push_socket,
    ratio,
};
use crate::tools::{ToolError, strict_object};

pub(crate) fn string_schema() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": 4096 })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the schema takes ownership of its generated properties object"
)]
pub(crate) fn schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": required,
        }
    })
}

pub(crate) fn launch_schema() -> Value {
    schema(
        "phux_launch",
        "Launch a configured agent integration, optionally placing it beside an exact local pane.",
        json!({
            "integration": string_schema(),
            "list": { "type": "boolean" },
            "target": string_schema(),
            "split": { "type": "string", "enum": ["horizontal", "vertical"] },
            "ratio": { "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1 },
            "cwd": string_schema(),
            "extra": { "type": "array", "maxItems": 64, "items": string_schema() },
            "socket": string_schema(),
        }),
        &[],
    )
}

pub(crate) fn spawn_schema() -> Value {
    schema(
        "phux_spawn",
        "Spawn a Terminal without attaching; optional target/split/ratio performs explicit local placement.",
        json!({
            "target": string_schema(),
            "satellite": string_schema(),
            "split": { "type": "string", "enum": ["horizontal", "vertical"] },
            "ratio": { "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1 },
            "cwd": string_schema(),
            "command": { "type": "array", "maxItems": 64, "items": string_schema() },
            "socket": string_schema(),
        }),
        &[],
    )
}

pub(crate) fn signal_schema() -> Value {
    schema(
        "phux_signal",
        "Send a process-group signal to exactly one target. terminate/kill/interrupt require confirm=true.",
        json!({
            "target": string_schema(),
            "signal": { "type": "string", "enum": ["interrupt", "freeze", "resume", "terminate", "kill"] },
            "confirm": { "type": "boolean", "description": "Required true for interrupt, terminate, and kill." },
            "socket": string_schema(),
        }),
        &["target", "signal"],
    )
}

pub(crate) fn tag_schema() -> Value {
    schema(
        "phux_tag",
        "List, add, or remove Terminal tags through the canonical phux tag command.",
        json!({
            "action": { "type": "string", "enum": ["ls", "add", "rm"] },
            "target": string_schema(),
            "tags": { "type": "array", "maxItems": 64, "items": string_schema() },
            "socket": string_schema(),
        }),
        &["action", "target"],
    )
}

pub(crate) fn rename_schema() -> Value {
    schema(
        "phux_rename",
        "Rename an existing session.",
        json!({ "session": string_schema(), "new_name": string_schema(), "socket": string_schema() }),
        &["session", "new_name"],
    )
}

fn spatial_schema(name: &str, description: &str, roles: &[&str], geometry: bool) -> Value {
    let mut properties = Map::new();
    for role in roles {
        properties.insert((*role).to_owned(), string_schema());
    }
    if geometry {
        properties.insert(
            "direction".to_owned(),
            json!({ "type": "string", "enum": ["horizontal", "vertical"] }),
        );
        properties.insert(
            "ratio".to_owned(),
            json!({ "type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1 }),
        );
    }
    properties.insert("socket".to_owned(), string_schema());
    schema(name, description, Value::Object(properties), roles)
}

pub(crate) fn insert_schema() -> Value {
    spatial_schema(
        "phux_insert_pane",
        "Insert an already-created local pane beside an exact same-session target; never spawns.",
        &["target", "new_pane"],
        true,
    )
}

pub(crate) fn move_schema() -> Value {
    spatial_schema(
        "phux_move_pane",
        "Move one exact local pane beside another, including across sessions.",
        &["source", "target"],
        true,
    )
}

pub(crate) fn swap_schema() -> Value {
    spatial_schema(
        "phux_swap_pane",
        "Swap two exact local pane leaves in the same session without changing focus.",
        &["first", "second"],
        false,
    )
}

pub(crate) fn workspace_schema() -> Value {
    schema(
        "phux_workspace",
        "Inspect a git workspace, save the running session archive, or restore an archive.",
        json!({
            "action": { "type": "string", "enum": ["inspect", "save", "restore"] },
            "path": string_schema(),
            "archive": string_schema(),
            "socket": string_schema(),
        }),
        &["action"],
    )
}

pub(crate) async fn call(name: &str, args: &Value) -> Result<Value, ToolError> {
    call_with_adapter(name, args, &CliAdapter::discover()).await
}

async fn call_with_adapter(
    name: &str,
    args: &Value,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    match name {
        "phux_launch" => launch(args, adapter).await,
        "phux_spawn" => spawn(args, adapter).await,
        "phux_signal" => signal(args, adapter).await,
        "phux_tag" => tag(args, adapter).await,
        "phux_rename" => rename(args, adapter).await,
        "phux_insert_pane" => {
            spatial(args, "insert-pane", &["target", "new_pane"], true, adapter).await
        }
        "phux_move_pane" => spatial(args, "move-pane", &["source", "target"], true, adapter).await,
        "phux_swap_pane" => spatial(args, "swap-pane", &["first", "second"], false, adapter).await,
        "phux_workspace" => workspace(args, adapter).await,
        other => Err(ToolError::new(format!("unknown CLI parity tool: {other}"))),
    }
}

async fn launch(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "integration",
            "list",
            "target",
            "split",
            "ratio",
            "cwd",
            "extra",
            "socket",
        ],
        &[],
    )?;
    let list = optional_bool(args, "list")?.unwrap_or(false);
    let integration = bounded_string(args, "integration", false)?;
    if list == integration.is_some() {
        return Err(ToolError::new(
            "provide exactly one of `integration` or `list: true`",
        ));
    }
    let target = bounded_string(args, "target", false)?;
    let split = enum_string(
        args,
        "split",
        &["horizontal", "vertical"],
        Some("horizontal"),
    )?;
    let ratio = ratio(args)?;
    if target.is_none() && (args.get("split").is_some() || ratio.is_some()) {
        return Err(ToolError::new("`split` and `ratio` require `target`"));
    }
    let mut argv = vec!["launch".to_owned()];
    if list {
        reject_present(
            args,
            &["target", "split", "ratio", "cwd", "extra", "socket"],
        )?;
        argv.extend(["--list".to_owned(), "--json".to_owned()]);
    } else {
        argv.push(integration.unwrap_or_default());
        argv.push("--json".to_owned());
        push_placement(&mut argv, target, &split, ratio);
        push_option(&mut argv, "-c", bounded_string(args, "cwd", false)?);
        push_socket(&mut argv, args)?;
        let extra = bounded_strings(args, "extra", false)?;
        if !extra.is_empty() {
            argv.push("--".to_owned());
            argv.extend(extra);
        }
    }
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn spawn(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &[
            "target",
            "satellite",
            "split",
            "ratio",
            "cwd",
            "command",
            "socket",
        ],
        &[],
    )?;
    let target = bounded_string(args, "target", false)?;
    let satellite = bounded_string(args, "satellite", false)?;
    if target.is_some() && satellite.is_some() {
        return Err(ToolError::new("`target` conflicts with `satellite`"));
    }
    let split = enum_string(
        args,
        "split",
        &["horizontal", "vertical"],
        Some("horizontal"),
    )?;
    let ratio = ratio(args)?;
    if target.is_none() && (args.get("split").is_some() || ratio.is_some()) {
        return Err(ToolError::new("`split` and `ratio` require `target`"));
    }
    let mut argv = vec!["spawn".to_owned(), "--json".to_owned()];
    push_placement(&mut argv, target, &split, ratio);
    push_option(&mut argv, "--satellite", satellite);
    push_option(&mut argv, "-c", bounded_string(args, "cwd", false)?);
    push_socket(&mut argv, args)?;
    let command = bounded_strings(args, "command", false)?;
    if !command.is_empty() {
        argv.push("--".to_owned());
        argv.extend(command);
    }
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn signal(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["target", "signal", "confirm", "socket"],
        &["target", "signal"],
    )?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let signal = enum_string(
        args,
        "signal",
        &["interrupt", "freeze", "resume", "terminate", "kill"],
        None,
    )?;
    let confirm = optional_bool(args, "confirm")?.unwrap_or(false);
    if matches!(signal.as_str(), "interrupt" | "terminate" | "kill") && !confirm {
        return Err(ToolError::new(format!(
            "signal {signal:?} is destructive; pass `confirm: true`"
        )));
    }
    let mut argv = vec!["signal".to_owned(), target.clone(), signal.clone()];
    push_socket(&mut argv, args)?;
    adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    Ok(json!({
        "schema_version": 1,
        "signaled": true,
        "target": target,
        "signal": signal,
    }))
}

async fn tag(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["action", "target", "tags", "socket"],
        &["action", "target"],
    )?;
    let action = enum_string(args, "action", &["ls", "add", "rm"], None)?;
    let target = bounded_string(args, "target", true)?.unwrap_or_default();
    let tags = bounded_strings(args, "tags", false)?;
    if action == "ls" && !tags.is_empty() {
        return Err(ToolError::new("`tags` is not accepted for action `ls`"));
    }
    if action != "ls" && tags.is_empty() {
        return Err(ToolError::new("`tags` is required for add/rm"));
    }
    let mut argv = vec!["tag".to_owned(), action.clone(), target];
    argv.extend(tags);
    push_socket(&mut argv, args)?;
    let output = adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    let terminals = parse_tag_lines(&output.stdout)?;
    Ok(json!({ "schema_version": 1, "action": action, "terminals": terminals }))
}

async fn rename(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(
        args,
        &["session", "new_name", "socket"],
        &["session", "new_name"],
    )?;
    let session = bounded_string(args, "session", true)?.unwrap_or_default();
    let new_name = bounded_string(args, "new_name", true)?.unwrap_or_default();
    let mut argv = vec!["rename".to_owned(), session.clone(), new_name.clone()];
    push_socket(&mut argv, args)?;
    adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
    Ok(json!({
        "schema_version": 1,
        "renamed": { "from": session, "to": new_name },
    }))
}

async fn spatial(
    args: &Value,
    command: &str,
    roles: &[&str],
    geometry: bool,
    adapter: &CliAdapter,
) -> Result<Value, ToolError> {
    let mut allowed = roles.to_vec();
    allowed.push("socket");
    if geometry {
        allowed.extend(["direction", "ratio"]);
    }
    strict_object(args, &allowed, roles)?;
    let mut argv = vec![command.to_owned()];
    for role in roles {
        argv.push(bounded_string(args, role, true)?.unwrap_or_default());
    }
    if geometry {
        let direction = enum_string(
            args,
            "direction",
            &["horizontal", "vertical"],
            Some("horizontal"),
        )?;
        argv.push(format!("--{direction}"));
        if let Some(ratio) = ratio(args)? {
            argv.extend(["--ratio".to_owned(), ratio.to_string()]);
        }
    }
    argv.push("--json".to_owned());
    push_socket(&mut argv, args)?;
    adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
}

async fn workspace(args: &Value, adapter: &CliAdapter) -> Result<Value, ToolError> {
    strict_object(args, &["action", "path", "archive", "socket"], &["action"])?;
    let action = enum_string(args, "action", &["inspect", "save", "restore"], None)?;
    let mut argv = vec!["workspace".to_owned(), action.clone()];
    match action.as_str() {
        "inspect" => {
            reject_present(args, &["archive", "socket"])?;
            if let Some(path) = bounded_string(args, "path", false)? {
                argv.push(path);
            }
            argv.push("--json".to_owned());
            adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
        }
        "save" => {
            reject_present(args, &["path", "archive"])?;
            push_socket(&mut argv, args)?;
            adapter.run_json(argv, DEFAULT_CALL_TIMEOUT).await
        }
        "restore" => {
            reject_present(args, &["path"])?;
            let archive = bounded_string(args, "archive", true)?.unwrap_or_default();
            if archive == "-" {
                return Err(ToolError::new(
                    "stdin archive input is unavailable over MCP; provide a bounded path",
                ));
            }
            argv.push(archive.clone());
            push_socket(&mut argv, args)?;
            adapter.run(argv, DEFAULT_CALL_TIMEOUT).await?;
            Ok(json!({ "schema_version": 1, "restored": true, "archive": archive }))
        }
        _ => Err(ToolError::new("unsupported workspace action")),
    }
}

fn push_placement(argv: &mut Vec<String>, target: Option<String>, split: &str, ratio: Option<f64>) {
    if let Some(target) = target {
        argv.extend([
            "--target".to_owned(),
            target,
            "--split".to_owned(),
            split.to_owned(),
        ]);
        if let Some(ratio) = ratio {
            argv.extend(["--ratio".to_owned(), ratio.to_string()]);
        }
    }
}

pub(crate) fn push_option(argv: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        argv.extend([flag.to_owned(), value]);
    }
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, ToolError> {
    args.get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| ToolError::new(format!("`{key}` must be a boolean")))
        })
        .transpose()
}

fn reject_present(args: &Value, keys: &[&str]) -> Result<(), ToolError> {
    keys.iter()
        .find(|key| args.get(**key).is_some())
        .map_or(Ok(()), |key| {
            Err(ToolError::new(format!(
                "`{key}` is not valid for this action"
            )))
        })
}

fn parse_tag_lines(stdout: &str) -> Result<Vec<Value>, ToolError> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (terminal, tags) = line
                .split_once('\t')
                .ok_or_else(|| ToolError::new("phux tag returned malformed output"))?;
            Ok(json!({
                "terminal": terminal,
                "tags": tags.split_whitespace().collect::<Vec<_>>(),
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    fn fake_cli() -> (TempDir, CliAdapter, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("argv");
        let executable = temp.path().join("phux");
        let script = format!(
            r#"#!/bin/sh
: > '{}'
for arg in "$@"; do
  printf '%s\n' "$arg" >> '{}'
done
case "$1" in
  tag) printf '@1\talpha\n' ;;
  agent) printf '@1\t{{"name":"bot"}}\n' ;;
  *) printf '{{}}\n' ;;
esac
"#,
            log.display(),
            log.display(),
        );
        fs::write(&executable, script).unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        (temp, CliAdapter::new(executable), log)
    }

    async fn assert_argv(
        adapter: &CliAdapter,
        log: &Path,
        name: &str,
        args: Value,
        expected: &[&str],
    ) {
        call_with_adapter(name, &args, adapter).await.unwrap();
        let actual = fs::read_to_string(log).unwrap();
        assert_eq!(actual.lines().collect::<Vec<_>>(), expected, "{name}");
    }

    #[test]
    fn every_added_schema_is_strict_and_bounded() {
        for schema in [
            launch_schema(),
            spawn_schema(),
            signal_schema(),
            tag_schema(),
            rename_schema(),
            insert_schema(),
            move_schema(),
            swap_schema(),
            workspace_schema(),
        ] {
            assert_eq!(schema["inputSchema"]["additionalProperties"], false);
            assert_eq!(schema["inputSchema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn dispatch_validation_blocks_dangerous_or_malformed_calls_before_execution() {
        let adapter = CliAdapter::new("must-not-execute");
        assert!(
            signal(&json!({ "target": "@1", "signal": "kill" }), &adapter)
                .await
                .is_err()
        );
        assert!(
            spawn(&json!({ "target": "@1", "satellite": "edge" }), &adapter,)
                .await
                .is_err()
        );
        assert!(
            spatial(
                &json!({ "source": "@1", "target": "@2", "extra": true }),
                "move-pane",
                &["source", "target"],
                true,
                &adapter,
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table-like execution test keeps exact argv evidence for all ten parity tools together"
    )]
    async fn every_added_handler_executes_the_exact_canonical_argv() {
        let (_temp, adapter, log) = fake_cli();

        assert_argv(
            &adapter,
            &log,
            "phux_launch",
            json!({
                "integration": "codex", "target": "@1", "split": "vertical",
                "ratio": 0.25, "cwd": "/work", "socket": "/sock",
                "extra": ["--model", "x"]
            }),
            &[
                "launch", "codex", "--json", "--target", "@1", "--split", "vertical", "--ratio",
                "0.25", "-c", "/work", "--socket", "/sock", "--", "--model", "x",
            ],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_spawn",
            json!({
                "target": "@2", "split": "horizontal", "ratio": 0.4,
                "cwd": "/repo", "socket": "/sock", "command": ["cargo", "test"]
            }),
            &[
                "spawn",
                "--json",
                "--target",
                "@2",
                "--split",
                "horizontal",
                "--ratio",
                "0.4",
                "-c",
                "/repo",
                "--socket",
                "/sock",
                "--",
                "cargo",
                "test",
            ],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_signal",
            json!({ "target": "@3", "signal": "kill", "confirm": true, "socket": "/sock" }),
            &["signal", "@3", "kill", "--socket", "/sock"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_tag",
            json!({ "action": "add", "target": "@4", "tags": ["build", "ci"], "socket": "/sock" }),
            &["tag", "add", "@4", "build", "ci", "--socket", "/sock"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_rename",
            json!({ "session": "old", "new_name": "new", "socket": "/sock" }),
            &["rename", "old", "new", "--socket", "/sock"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_insert_pane",
            json!({ "target": "@6", "new_pane": "@7", "direction": "vertical", "ratio": 0.3, "socket": "/sock" }),
            &["insert-pane", "@6", "@7", "--vertical", "--ratio", "0.3", "--json", "--socket", "/sock"],
        ).await;
        assert_argv(
            &adapter,
            &log,
            "phux_move_pane",
            json!({ "source": "@7", "target": "@8", "direction": "horizontal", "ratio": 0.6, "socket": "/sock" }),
            &["move-pane", "@7", "@8", "--horizontal", "--ratio", "0.6", "--json", "--socket", "/sock"],
        ).await;
        assert_argv(
            &adapter,
            &log,
            "phux_swap_pane",
            json!({ "first": "@8", "second": "@9", "socket": "/sock" }),
            &["swap-pane", "@8", "@9", "--json", "--socket", "/sock"],
        )
        .await;
        assert_argv(
            &adapter,
            &log,
            "phux_workspace",
            json!({ "action": "inspect", "path": "/repo" }),
            &["workspace", "inspect", "/repo", "--json"],
        )
        .await;
    }

    #[test]
    fn canonical_text_parsers_are_strict() {
        assert_eq!(
            parse_tag_lines("@1\tbuild ci\n").unwrap(),
            vec![json!({ "terminal": "@1", "tags": ["build", "ci"] })]
        );
        assert!(parse_tag_lines("not-tabbed").is_err());
    }
}
