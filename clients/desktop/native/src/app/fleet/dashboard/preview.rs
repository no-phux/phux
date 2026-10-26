//! Native design-review captures using the shipping composition. Never dials a host.

use super::super::{Agent, Destination};
use crate::app::desk::{Desk, Modal};
use crate::connection::{AgentAttention, AgentInfo, AgentState};
use gpui::{App, AppContext as _, px, size};
use std::{path::PathBuf, time::Duration};

fn inventory() -> Vec<Agent> {
    [
        (
            "Claude",
            "claude",
            "Reviewing the authentication boundary",
            None,
            "phux / API",
            AgentState::Working,
        ),
        (
            "Pi",
            "pi",
            "Ship the new fleet experience",
            None,
            "phux / Desktop",
            AgentState::Working,
        ),
        (
            "Codex",
            "codex",
            "Approval needed before running migration",
            Some("studio"),
            "platform / Database",
            AgentState::Blocked,
        ),
        (
            "OpenCode",
            "opencode",
            "Keyboard navigation · regression suite",
            Some("studio"),
            "phux / Tests",
            AgentState::Done,
        ),
        (
            "Gemini",
            "gemini",
            "Explore the next release",
            Some("build-linux"),
            "research",
            AgentState::Idle,
        ),
        (
            "Grok",
            "grok",
            "Waiting for the next instruction",
            Some("build-linux"),
            "docs / Guides",
            AgentState::Idle,
        ),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (name, kind, title, host, session, state))| Agent {
        destination: Destination {
            host: host.map(str::to_owned),
            authority: None,
            key: None,
            name: session.into(),
        },
        terminal: phux_protocol::ResourceId::local(index as u32 + 1),
        boot: Some("fixture".into()),
        info: AgentInfo {
            name: name.into(),
            kind: Some(kind.into()),
            state,
            attention: AgentAttention::None,
            changed_at_ms: 1,
        },
        title: title.into(),
    })
    .collect()
}

pub(crate) fn capture(
    root: gpui::WindowHandle<gpui_kit::component::Root>,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.spawn(async move |cx| {
        for (name, width, height, light, empty, playground) in [
            ("fleet-dark", 1280., 900., false, false, false),
            ("fleet-light", 1280., 900., true, false, false),
            ("fleet-narrow", 520., 780., false, false, false),
            ("fleet-empty", 900., 700., false, true, false),
            ("fleet-playground", 1280., 900., false, false, true),
            ("fleet-playground-light", 1280., 900., true, false, true),
        ] {
            root.update(cx, |root, window, cx| {
                let desk = root.view().clone().downcast::<Desk>().expect("desk");
                window.resize(size(px(width), px(height)));
                desk.update(cx, |desk, cx| {
                    let theme = crate::settings::theme::themes()
                        .iter()
                        .find(|theme| (theme.appearance == "light") == light)
                        .expect("theme")
                        .id
                        .clone();
                    desk.update_prefs(
                        |prefs| {
                            prefs.agent_fleet = true;
                            prefs.animations = false;
                            prefs.theme_id = theme;
                        },
                        cx,
                    );
                    desk.fleet_view.open();
                    desk.fleet_view.playground.on = playground;
                    desk.fleet_view.fixture = Some(if empty { Vec::new() } else { inventory() });
                    desk.modal = Modal::AgentFleet;
                    cx.notify();
                });
                window.refresh();
            })
            .expect("fixture window");
            cx.background_executor()
                .timer(Duration::from_millis(800))
                .await;
            cx.update_window(*root, |_, window, cx| window.draw(cx).clear(cx))
                .expect("draw fleet");
            root.update(cx, |_, window, _| {
                window
                    .render_to_image()
                    .expect("fleet scene capture")
                    .save(directory.join(format!("{name}.png")))
                    .expect("write fleet PNG");
            })
            .expect("capture fleet");
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}
