//! `phux agent surface targets`: the displays and windows `phux agent
//! surface mirror` can name on this host, for a picker. Linux reads
//! Hyprland (`hyprctl monitors|clients`; any other compositor offers only
//! `display`); macOS lists on-screen windows, whose titles only a process
//! with Screen Recording access sees, so Rune runs it there.

use std::process::ExitCode;

use serde::Serialize;

use crate::commands::json_err;
use crate::exit_codes::EXIT_FAILURE;

/// One thing a mirror can be pointed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CaptureTarget {
    /// What `phux agent surface mirror` takes.
    pub target: String,
    /// `display` or `window`.
    pub kind: &'static str,
    /// What a picker shows.
    pub label: String,
}

pub(super) fn run_targets(json: bool) -> ExitCode {
    match list() {
        Ok(targets) if json => {
            outln!(
                "{}",
                serde_json::json!({ "schema_version": 1, "targets": targets })
            );
            ExitCode::SUCCESS
        }
        Ok(targets) => {
            for t in targets {
                outln!("{}\t{}\t{}", t.kind, t.target, t.label);
            }
            ExitCode::SUCCESS
        }
        Err(message) => json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::WORKLOAD,
                message,
                "capture targets are listed on macOS and on Linux under Hyprland",
            ),
            EXIT_FAILURE,
        ),
    }
}

/// This host's capture targets.
#[cfg_attr(
    target_os = "linux",
    allow(clippy::unnecessary_wraps, reason = "fails on macOS")
)]
pub(crate) fn list() -> Result<Vec<CaptureTarget>, String> {
    #[cfg(target_os = "linux")]
    {
        let env = super::surface_grim::session_env();
        let hyprland = super::surface_grim::hyprctl("monitors", &env).and_then(|monitors| {
            from_hyprland(&monitors, &super::surface_grim::hyprctl("clients", &env)?)
        });
        Ok(hyprland.unwrap_or_else(|_| vec![all_displays("Every output")]))
    }
    #[cfg(target_os = "macos")]
    {
        from_macos(&super::surface_mirror::macos::window_list()?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err("screen mirrors run on macOS and Linux only".to_owned())
    }
}

fn all_displays(label: &str) -> CaptureTarget {
    CaptureTarget {
        target: "display".to_owned(),
        kind: "display",
        label: label.to_owned(),
    }
}

fn parse(text: &str, what: &str) -> Result<Vec<serde_json::Value>, String> {
    serde_json::from_str(text.trim()).map_err(|err| format!("{what} were not readable: {err}"))
}

/// Every output, each output, then each mapped window, from `hyprctl -j
/// monitors` and `hyprctl -j clients`.
#[cfg(any(target_os = "linux", test))]
fn from_hyprland(monitors: &str, clients: &str) -> Result<Vec<CaptureTarget>, String> {
    let mut targets = vec![all_displays("Every output")];
    for m in parse(monitors, "hyprctl's monitors")? {
        let Some(name) = m["name"].as_str() else {
            continue;
        };
        let description = m["description"].as_str().unwrap_or_default();
        targets.push(CaptureTarget {
            target: format!("display:{name}"),
            kind: "display",
            label: if description.is_empty() {
                name.to_owned()
            } else {
                format!("{name} ({description})")
            },
        });
    }
    let shown = parse(clients, "hyprctl's clients")?
        .into_iter()
        .filter(|c| c["mapped"].as_bool() != Some(false) && c["hidden"].as_bool() != Some(true))
        .filter_map(|c| {
            let app = c["class"].as_str().filter(|s| !s.is_empty())?.to_owned();
            Some((app, c["title"].as_str().unwrap_or_default().to_owned()))
        });
    targets.extend(windows(&shown.collect::<Vec<_>>()));
    Ok(targets)
}

/// The main display, then each normal-layer window, from the macOS window
/// list (`surface_mirror::macos::window_list`).
#[cfg(any(target_os = "macos", test))]
fn from_macos(listing: &str) -> Result<Vec<CaptureTarget>, String> {
    let shown = parse(listing, "the window list")?
        .into_iter()
        .filter(|w| w["layer"].as_i64().unwrap_or(0) == 0)
        .filter_map(|w| {
            let app = w["owner"].as_str().filter(|s| !s.is_empty())?.to_owned();
            Some((app, w["name"].as_str().unwrap_or_default().to_owned()))
        });
    let mut targets = vec![all_displays("Main display")];
    targets.extend(windows(&shown.collect::<Vec<_>>()));
    Ok(targets)
}

/// A window as `App` when it is its app's only one (a title that changes,
/// like a browser tab's, keeps matching), else `App:title`; untitled extra
/// windows of an app are left out, since nothing names them apart.
fn windows(shown: &[(String, String)]) -> Vec<CaptureTarget> {
    let count = |app: &str| shown.iter().filter(|(a, _)| a == app).count();
    let mut targets: Vec<CaptureTarget> = Vec::new();
    for (app, title) in shown {
        let target = if count(app) == 1 {
            app.clone()
        } else if title.is_empty() {
            continue;
        } else {
            format!("{app}:{title}")
        };
        if targets.iter().any(|t| t.target == target) {
            continue;
        }
        targets.push(CaptureTarget {
            target,
            kind: "window",
            label: if title.is_empty() {
                app.clone()
            } else {
                format!("{app}: {title}")
            },
        });
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_lists_outputs_then_mapped_windows() {
        let targets = from_hyprland(
            include_str!("fixtures/hyprctl-monitors.json"),
            include_str!("fixtures/hyprctl-clients.json"),
        )
        .expect("parse");
        let names: Vec<_> = targets.iter().map(|t| t.target.as_str()).collect();
        assert_eq!(
            names,
            [
                "display",
                "display:DP-1",
                "display:HDMI-A-1",
                "firefox:phux - GitHub — Mozilla Firefox",
                "firefox:Docs: mirrors — Mozilla Firefox",
                "com.mitchellh.ghostty",
            ]
        );
        assert_eq!(targets[1].label, "DP-1 (Dell Inc. DELL U2720Q 1234)");
        assert_eq!(targets[5].kind, "window");
        assert!(from_hyprland("not json", "[]").is_err());
    }

    #[test]
    fn macos_lists_the_main_display_and_normal_windows() {
        let listing = r#"[{"id":7,"owner":"Finder","name":"","layer":0},
            {"id":9,"owner":"Safari","name":"Docs","layer":0},
            {"id":10,"owner":"Safari","name":"","layer":0},
            {"id":11,"owner":"Safari","name":"Menu","layer":25}]"#;
        let targets = from_macos(listing).expect("parse");
        let names: Vec<_> = targets.iter().map(|t| t.target.as_str()).collect();
        assert_eq!(names, ["display", "Finder", "Safari:Docs"]);
    }
}
