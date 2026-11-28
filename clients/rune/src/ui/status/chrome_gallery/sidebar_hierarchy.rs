//! Actual project, child-pane and machine hierarchy, including native chrome room.
use super::*;
use phux_client_runtime::project_identity::SessionProject;

mod current_member;
mod row_alignment;
mod tests;

fn inventory() -> phux_client_runtime::control::Topology {
    let mut main = session(1, "phux", 1);
    let mut branch = session(2, "feat/sidebar-hierarchy", 2);
    let project = |linked| SessionProject {
        id: "github.com/no-phux/phux".to_owned(),
        name: "phux".to_owned(),
        checkout: "/workspace/phux".to_owned(),
        branch: None,
        linked_worktree: linked,
        host: None,
    };
    main.project = Some(project(false));
    branch.project = Some(project(true));
    let deploy = session(3, "Release engineering", 3);
    let notes = session(4, "Notes", 4);
    let mut panes = vec![
        pane(1, &main, "Polish session navigation", None),
        pane(2, &main, "Review hierarchy and accessibility", None),
        pane(3, &main, "Verify keyboard routing", None),
        pane(4, &branch, "Inspect remote-agent permissions", None),
        pane(5, &deploy, "Tail deployment logs", None),
    ];
    for child in &mut panes[1..3] {
        child.parent = Some(ResourceId::local(1));
    }
    let mut topology = crate::desk::topology(vec![main, branch, deploy, notes], panes);
    server::terminal_resources(&mut topology);
    topology
}

fn machines(desk: &mut Desk) {
    desk.machines.hosts = ["build-mini", "staging"]
        .map(|name| crate::machines::Host {
            name: name.to_owned(),
            ssh: None,
            ssh_only: false,
            paused: false,
        })
        .to_vec();
    desk.machines.glances.insert(
        Some("build-mini".to_owned()),
        Ok(vec![crate::machines::Glance {
            name: "Release qualification".to_owned(),
            terminals: 3,
            running: 1,
        }]),
    );
}

fn window(size: (u16, u16), dark: bool, mode: &str) -> Runtime<Desk> {
    window_with_replica(
        size,
        dark,
        mode,
        crate::test_support::terminal::Replica::new(120, 40, 5),
    )
}

fn window_with_replica(
    size: (u16, u16),
    dark: bool,
    mode: &str,
    mut replica: crate::test_support::terminal::Replica,
) -> Runtime<Desk> {
    replica.feed("\x1b[2J\x1b[H$ cargo test --locked\r\n   Compiling phux-rune\r\n".to_owned());
    let mut desk = replica.window();
    desk.connection.shown = Some(crate::desk::Shown {
        resource: replica.terminal.clone(),
        view: Some(replica.views[0]),
    });
    desk.connection.layout = Some(LayoutNode::Leaf(replica.terminal));
    desk.connection.topology = Some(inventory());
    desk.connection.status = phux_client_runtime::control::Status::Attached;
    desk.sidebar_open = true;
    desk.sidebar_peek = true;
    machines(&mut desk);
    for (id, kind, state) in [
        (1, "claude", "working"),
        (2, "opencode", "blocked"),
        (3, "codex", "failed"),
        (4, "claude", "done"),
        (5, "opencode", "idle"),
    ] {
        desk.activities.apply(&agent(id, kind, state), &[]);
    }
    if mode == "large" {
        desk.set_interface_size(Some(140));
    }
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (f32::from(size.0), f32::from(size.1)),
        Env::new(dark, scale()),
    );
    rt.on_key = Some(Desk::key);
    rt.poll = Some(|d, _, tx| d.return_keyboard(tx));
    rt.set_system_reduced_motion(true);
    // The actual app positions macOS controls at (14,14), on Rune's 23pt pitch.
    rt.set_titlebar_inset(
        rune::Chrome {
            full_size_content: true,
            traffic_lights: Some((14.0, 14.0)),
            drag_regions: true,
        }
        .titlebar_inset(),
    );
    if mode == "forced" {
        rt.set_system_contrast(Some(crate::test_support::visual::forced_colors(dark)));
    }
    server::settle(&mut rt);
    // First layout folds a narrow window. Open its actual drawer afterward,
    // rather than trying to press retained elements behind the terminal.
    if !rt.state.sidebar_shown() {
        rt.update(|d, _| d.toggle_sidebar());
        server::settle(&mut rt);
    }
    assert!(rt.state.sidebar_shown());
    assert!(rt.find("machine-switch").is_some());
    rt
}

fn press(rt: &mut Runtime<Desk>, key: &str) {
    rt.update(|_, tx| tx.scroll_into_view(key));
    server::settle(rt);
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    let picked = rt.element_at(x, y).expect("a visible hierarchy target");
    assert!(
        picked.path().split('/').any(|part| part == key),
        "{key} is covered by {picked:?}"
    );
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(3.0);
    // A completed disclosure scene is not a hover-tooltip scene.
    rt.pointer_move(0.0, 0.0);
    server::settle(rt);
}

fn state(rt: &mut Runtime<Desk>, state: &str) {
    match state {
        "single" => {
            rt.update(|d, _| {
                let top = d.connection.topology.as_mut().unwrap();
                top.sessions.retain(|s| s.id == 1);
                top.panes.retain(|p| p.terminal_id == ResourceId::local(1));
                top.resources.retain(|r| r.id == ResourceId::local(1));
            });
        }
        "split" => {
            rt.update(|d, _| {
                d.connection.tabs.by_session.insert(
                    1,
                    vec![Tab {
                        id: [1; 16],
                        name: "Polish session navigation".to_owned(),
                        resources: [1, 2, 3].map(ResourceId::local).to_vec(),
                        active: true,
                        locked: false,
                    }],
                );
            });
        }
        "hosts" => {
            press(rt, "host-build-mini");
            assert!(
                rt.state
                    .machines
                    .expanded
                    .contains(&Some("build-mini".to_owned()))
            );
            rt.update(|_, tx| tx.scroll_to_instant("sidebar-scroll", 0.0, 10_000.0));
        }
        "focused" => {
            super::sidebar_focus::focus_sidebar(rt);
            assert!(crate::ui::element(rt, "pane-@1").focused);
        }
        "empty" => rt.update(|d, _| {
            d.connection.topology = Some(crate::desk::topology(Vec::new(), Vec::new()));
            d.connection.shown = None;
        }),
        "expanded" => {}
        other => panic!("unknown hierarchy scene {other}"),
    }
    server::settle(rt);
}

#[test]
fn sidebar_hierarchy_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for size in [(1100, 720), (640, 360)] {
        for (dark, theme) in [(true, "dark"), (false, "light")] {
            for mode in ["normal", "large", "forced"] {
                for scene in ["expanded", "single", "split", "hosts", "focused", "empty"] {
                    let mut rt = window(size, dark, mode);
                    state(&mut rt, scene);
                    let format = rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix();
                    let mut renderer = rune::render::Renderer::new(&gpu.device, format);
                    renderer.set_srgb_blend(true);
                    let name = format!(
                        "sidebar-hierarchy-{theme}-{mode}-{scene}-{}x{}-2x",
                        size.0, size.1
                    );
                    super::sidebar_focus::capture(&gpu, &mut renderer, &mut rt, size, &name);
                }
            }
        }
    }
}
