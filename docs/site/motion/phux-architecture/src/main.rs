//! Silent architecture film. Timings explain causality, not measured latency.
use psychopomp::{
    author::{PlanBuilder, seconds},
    score::{Beat, Caption, Stage},
    stage::{StageElement, StagePlan, StagePost},
    tone::Tone,
};

fn main() -> anyhow::Result<()> {
    let plan = StagePlan {
        post: StagePost::RESTRAINED,
        elements: vec![
            StageElement::label(
                "machine",
                [830.0, 220.0, 0.0],
                25.0,
                &[("LOCAL MACHINE", Tone::Muted)],
            ),
            StageElement::card("human", [285.0, 340.0, 0.0], [310.0, 110.0], "Human / CLI")
                .statuses(&[("attach, type, detach", Tone::Muted)]),
            StageElement::card("web", [285.0, 540.0, 0.0], [310.0, 110.0], "Desktop / web")
                .statuses(&[("another view", Tone::Muted)]),
            StageElement::card("agent", [285.0, 760.0, 0.0], [310.0, 110.0], "Agent")
                .statuses(&[("structured input", Tone::Muted)]),
            StageElement::card("server", [830.0, 340.0, 0.0], [340.0, 120.0], "phux server")
                .statuses(&[("owns terminal lifetime", Tone::Muted)])
                .tone(Tone::Accent),
            StageElement::card(
                "terminal",
                [830.0, 540.0, 0.0],
                [340.0, 120.0],
                "Persistent terminal",
            )
            .statuses(&[("state survives detach", Tone::Muted)])
            .tone(Tone::Accent),
            StageElement::card("pty", [830.0, 760.0, 0.0], [340.0, 110.0], "PTY / process")
                .statuses(&[("runs on this machine", Tone::Muted)]),
            StageElement::card(
                "hub",
                [1240.0, 340.0, 0.0],
                [275.0, 120.0],
                "Federation hub",
            )
            .statuses(&[("remote resources", Tone::Muted)]),
            StageElement::card(
                "remote",
                [1650.0, 340.0, 0.0],
                [300.0, 120.0],
                "Remote machine",
            )
            .statuses(&[("remote phux server", Tone::Muted)]),
            StageElement::card(
                "remote-process",
                [1650.0, 590.0, 0.0],
                [300.0, 120.0],
                "Remote terminal",
            )
            .statuses(&[("process stays here", Tone::Muted)]),
            StageElement::beam("human-link", "human", "server").tone(Tone::Accent),
            StageElement::beam("web-link", "web", "server").bend(-25.0),
            StageElement::beam("agent-link", "agent", "server")
                .bend(-35.0)
                .tone(Tone::Accent),
            StageElement::beam("ownership", "server", "terminal"),
            StageElement::beam("input-path", "server", "pty")
                .bend(-340.0)
                .tone(Tone::Accent),
            StageElement::beam("vt-path", "pty", "terminal").tone(Tone::Accent),
            StageElement::beam("hub-link", "server", "hub").tone(Tone::Accent),
            StageElement::beam("remote-link", "hub", "remote").tone(Tone::Accent),
            StageElement::beam("remote-ownership", "remote", "remote-process"),
            StageElement::packet("input", "human-link")
                .labeled("structured input")
                .tone(Tone::Accent),
            StageElement::packet("pty-input", "input-path")
                .labeled("input")
                .tone(Tone::Accent),
            StageElement::packet("vt", "vt-path")
                .labeled("VT bytes")
                .tone(Tone::Accent),
            StageElement::packet("agent-input", "agent-link")
                .labeled("structured input")
                .tone(Tone::Accent),
            StageElement::packet("resource", "hub-link")
                .labeled("remote resource")
                .tone(Tone::Accent),
            StageElement::packet("reach", "remote-link")
                .labeled("access")
                .tone(Tone::Accent),
            StageElement::packet("reply", "remote-link")
                .reversed()
                .labeled("remote view")
                .tone(Tone::Accent),
        ],
    };
    let mut scene = PlanBuilder::new("phux-architecture", seconds(16.0));
    let s = Stage::declare(&mut scene, "stage", &plan)?;
    let title = Caption::header(&mut scene, "phux", "One terminal. Many ways in.")?;
    let first = Caption::footer(
        &mut scene,
        "first",
        &[(
            "Clients send structured input. The PTY emits VT bytes.",
            Tone::Plain,
        )],
    )?;
    let second = Caption::footer(
        &mut scene,
        "second",
        &[(
            "Detach the client. The server still owns the terminal.",
            Tone::Plain,
        )],
    )?;
    let third = Caption::footer(
        &mut scene,
        "third",
        &[(
            "Federation exposes remote resources. It does not migrate processes.",
            Tone::Plain,
        )],
    )?;
    title.show().play(&mut scene, 0);
    first.show().play(&mut scene, seconds(0.3));
    s.fade_in("machine", 1.0, 0.4)
        .play(&mut scene, seconds(0.2));
    for (i, id) in ["server", "terminal", "pty", "human", "web", "agent"]
        .iter()
        .enumerate()
    {
        s.settle_in(*id)
            .play(&mut scene, seconds(0.2 + i as f64 * 0.12));
    }
    for id in [
        "human-link",
        "web-link",
        "agent-link",
        "ownership",
        "input-path",
        "vt-path",
    ] {
        s.connect(id, 0.5).play(&mut scene, seconds(1.1));
    }
    s.send("input", 0.8)
        .then(s.land("server"))
        .play(&mut scene, seconds(2.1));
    s.send("pty-input", 0.9)
        .then(s.land("pty"))
        .play(&mut scene, seconds(3.35));
    s.send("vt", 0.8)
        .then(s.land("terminal"))
        .play(&mut scene, seconds(4.65));
    first.hide().play(&mut scene, seconds(6.5));
    second.show().play(&mut scene, seconds(6.9));
    s.fade_out("human", 0.5).play(&mut scene, seconds(7.0));
    s.fade_out("human-link", 0.5).play(&mut scene, seconds(7.0));
    s.send("agent-input", 0.9)
        .then(s.land("server"))
        .play(&mut scene, seconds(8.1));
    second.hide().play(&mut scene, seconds(9.45));
    third.show().play(&mut scene, seconds(9.9));
    for (i, id) in ["hub", "remote", "remote-process"].iter().enumerate() {
        s.settle_in(*id)
            .play(&mut scene, seconds(9.3 + i as f64 * 0.12));
    }
    for id in ["hub-link", "remote-link", "remote-ownership"] {
        s.connect(id, 0.5).play(&mut scene, seconds(10.1));
    }
    s.send("resource", 0.7)
        .then(s.land("hub"))
        .play(&mut scene, seconds(10.8));
    s.send("reach", 0.7)
        .then(s.land("remote"))
        .play(&mut scene, seconds(11.9));
    s.send("reply", 0.7)
        .then(s.land("hub"))
        .play(&mut scene, seconds(13.0));
    scene.sort_events();
    std::fs::create_dir_all("target")?;
    std::fs::write(
        "target/phux-architecture.json",
        serde_json::to_string_pretty(&scene.finish()?)?,
    )?;
    Ok(())
}
