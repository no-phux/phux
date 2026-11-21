//! A populated server for the painted galleries: sessions, terminals, agent
//! reports, and the frame as the app shows it.

use phux_client_runtime::control::{
    Event, PaneDescriptor, ResourceDescriptor, SessionDescriptor, Topology,
};
use phux_protocol::ids::{ResourceId, ResourceKind, SessionKey};
use rune::Runtime;

use crate::desk::Desk;

pub fn session(id: u32, name: impl Into<String>, key: u8) -> SessionDescriptor {
    SessionDescriptor {
        id,
        name: name.into(),
        attached_client_count: 1,
        keep_empty: true,
        created_at_unix_secs: 0,
        key: SessionKey::new([key; 16]),
        touched: Some(u64::from(100 - id)),
        lifetime: None,
        revision: None,
        members: 0,
        arrangement: None,
        awaiting_organization: false,
        project: None,
        project_link: None,
    }
}

/// Terminal `id` in `session`, titled `title`, at `~/workspace/phux` unless
/// `cwd` says otherwise.
pub fn pane(
    id: u32,
    session: &SessionDescriptor,
    title: impl Into<String>,
    cwd: Option<&str>,
) -> PaneDescriptor {
    let title = title.into();
    PaneDescriptor {
        terminal_id: ResourceId::local(id),
        session_id: session.id,
        session_name: session.name.clone(),
        cols: 120,
        rows: 40,
        title: Some(title.clone()),
        cwd: Some(cwd.unwrap_or("~/workspace/phux").to_owned()),
        is_focused: id == 1,
        key: None,
        revision: None,
        name: Some(title),
        activation: None,
        exec: None,
        foreground: None,
        icon: None,
    }
}

/// An agent of `kind` reporting `state` in terminal `id`.
pub fn agent(id: u32, kind: &str, state: &str) -> Event {
    Event::AgentMetadata {
        terminal_id: ResourceId::local(id),
        value: Some(
            format!(r#"{{"name":"{kind}","kind":"{kind}","state":"{state}"}}"#).into_bytes(),
        ),
    }
}

/// Every pane of `topology` as a terminal resource.
pub fn terminal_resources(topology: &mut Topology) {
    topology.resources = (topology.panes.iter())
        .map(|p| ResourceDescriptor {
            id: p.terminal_id.clone(),
            kind: ResourceKind::Terminal,
            session_id: p.session_id,
            parent: None,
            title: p.title.clone(),
            cells: Some((120, 40)),
        })
        .collect();
}

/// Let the window's transitions finish.
pub fn settle(rt: &mut Runtime<Desk>) {
    for t in [0.0, 0.5, 1.0, 2.0, 3.0] {
        rt.frame(t);
    }
}

/// The settled frame as the app shows it, blended in sRGB, at `size`
/// points times `scale`: it must paint more than grey, and it is written
/// to `$<snap_var>/<file>.png` when that variable is set.
pub fn paint(
    gpu: &rune::render::Gpu,
    rt: &mut Runtime<Desk>,
    size: (f32, f32),
    scale: f32,
    snap_var: &str,
    file: &str,
) -> Vec<u8> {
    rt.set_srgb_blend(true);
    settle(rt);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixed size"
    )]
    let (w, h) = ((size.0 * scale) as u32, (size.1 * scale) as u32);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, (w, h));
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{file} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os(snap_var) {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        rune::render::write_png(&dir.join(format!("{file}.png")), w, h, &pixels).unwrap();
    }
    pixels
}
