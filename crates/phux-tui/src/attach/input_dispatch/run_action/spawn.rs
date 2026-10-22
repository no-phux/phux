//! New tabs and the directory and path pickers that choose where they open.

use std::collections::HashMap;

use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
use phux_protocol::ResourceId;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::PendingWindow;
use crate::attach::directory_picker::{DirectorySupport, ListingHost, PendingDirectory};
use crate::attach::input_dispatch::args::str_arg;
use crate::attach::input_dispatch::dispatch::spawn_initial_size;
use crate::attach::pane_state::PaneSlot;
use crate::render::overlay::PendingOverlay;

use super::{ActionCall, bind_satellite_spawn, spawn_place};

/// Open a new window: spawn a Terminal and park a `PendingWindow`; the reply
/// opens and activates the window. `cwd` starts the shell there; `host`
/// spawns on that satellite through the hub (`cwd` is then a satellite path).
pub(in crate::attach) fn new_window(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    let request_id = ctx.take_request_id();
    let name = ctx.workspace.default_group_name();
    let place = spawn_place(resolved, focused, panes);
    let mut frame = FrameKind::SpawnResource {
        request_id,
        group: DEFAULT_GROUP_ID,
        command: None,
        cwd: place.directory,
        env: None,
        term: None,
        satellite: place.host.map(SatelliteHost::new),
        owner_terminal: None,
        agent_session: None,
        // The new window holds one leaf, so the pane
        // fills the whole content rect. Predicting that here spares
        // the pane a bootstrap-then-reflow round trip.
        initial_size: spawn_initial_size(ctx, |content| Some((content.w, content.h))),
        resource: None,
    };
    bind_satellite_spawn(&mut frame);
    effects.spawn_window = Some((request_id, PendingWindow { name, adopt: None }, frame));
}

/// Request a directory listing (`docs/spec/L3.md` §4) on [`listing_host`],
/// starting at `path`, else the focused pane's cwd when it lives on that
/// host, else home. Bells when the server lacks `LIST_DIRECTORY`.
pub(in crate::attach) fn go_to_directory(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    if ctx.directory_support == DirectorySupport::Unsupported {
        tracing::warn!(
            "go-to-directory: server does not advertise LIST_DIRECTORY; dropping action"
        );
        effects.bell = true;
        return;
    }
    let host = listing_host(host_arg(resolved), focused, ctx.directory_support);
    let path = str_arg(resolved, "path")
        .or_else(|| pane_cwd_on(host.satellite(), focused, panes))
        .unwrap_or_default();
    let request_id = ctx.take_request_id();
    // Modal from the moment the request leaves: the placeholder swallows
    // keystrokes and Escape cancels, so nothing typed during a slow listing
    // reaches the pane and a cancelled listing never opens late.
    ctx.overlays.push(Box::new(PendingOverlay::listing(
        &placeholder_label(&host, &path),
        request_id,
        ctx.theme,
    )));
    effects.layout_mutated = true;
    let frame = FrameKind::ListDirectory {
        request_id,
        path,
        host: host.satellite().cloned(),
    };
    effects.list_directory = Some((PendingDirectory { request_id, host }, frame));
}

/// Browse a host path while preserving the pane captured at first open.
pub(in crate::attach) fn find_path(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    if !ctx.path_query_supported {
        effects.bell = true;
        return;
    }
    let Some(target) = focused else {
        effects.bell = true;
        return;
    };
    // A browse row must never recapture a different pane when focus or its
    // input lease moved during the overlay's lifetime.
    let old = ctx.pending_path.as_ref();
    if old.is_some_and(|pending| {
        !crate::attach::path_picker::may_insert(pending, focused, ctx.own_client_id, panes)
    }) {
        *ctx.pending_path = None;
        effects.bell = true;
        return;
    }
    let holder = old.map_or_else(
        || panes.get(target).and_then(|p| p.input_holder),
        |p| p.holder,
    );
    if holder.is_some_and(|id| Some(id) != ctx.own_client_id) || panes.get(target).is_none() {
        effects.bell = true;
        return;
    }
    let root = str_arg(resolved, "path")
        .or_else(|| pane_cwd_on(target.host(), focused, panes))
        .unwrap_or_default();
    let request_id = ctx.take_request_id();
    let pending = crate::attach::path_picker::PendingPath {
        target: target.clone(),
        holder,
        request_id,
        root: root.clone(),
        query: String::new(),
    };
    ctx.overlays
        .push(Box::new(crate::render::overlay::PathPicker::new(
            root.clone(),
            ctx.theme,
        )));
    effects.layout_mutated = true;
    effects.query_path = Some((
        pending,
        FrameKind::PathQuery {
            request_id,
            root,
            query: String::new(),
            recursive: false,
            host: target.host().cloned(),
        },
    ));
}

pub(in crate::attach) fn insert_path(
    ActionCall {
        resolved,
        ctx,
        focused,
        panes,
        effects,
    }: ActionCall<'_, '_>,
) {
    let Some(pending) = ctx.pending_path.take() else {
        effects.bell = true;
        return;
    };
    let Some(path) = str_arg(resolved, "path") else {
        effects.bell = true;
        return;
    };
    if !path.starts_with('/')
        || path.chars().any(char::is_control)
        || !crate::attach::path_picker::may_insert(&pending, focused, ctx.own_client_id, panes)
    {
        effects.bell = true;
        return;
    }
    effects.insert_path = Some((
        pending.target,
        crate::attach::path_picker::shell_quote(&path),
    ));
}

/// A non-empty `host` arg as a satellite name.
fn host_arg(resolved: &phux_config::keybind::ResolvedAction) -> Option<SatelliteHost> {
    str_arg(resolved, "host")
        .filter(|host| !host.is_empty())
        .map(SatelliteHost::new)
}

/// The host one listing reads: `wanted`, else the focused pane's satellite,
/// else the attached server. An older hub would list itself, so the request
/// stays there and the picker says so.
fn listing_host(
    wanted: Option<SatelliteHost>,
    focused: Option<&ResourceId>,
    support: DirectorySupport,
) -> ListingHost {
    let wanted = wanted.or_else(|| focused.and_then(ResourceId::host).cloned());
    match wanted {
        None => ListingHost::Attached,
        Some(host) if support == DirectorySupport::HostAware => ListingHost::Satellite(host),
        Some(host) => ListingHost::AttachedInsteadOf(host),
    }
}

/// The focused pane's working directory, when that pane lives on `host`
/// (`None` is the attached server). A pane on any other host names a path
/// the listed host does not have, so the listing starts at home instead.
fn pane_cwd_on(
    host: Option<&SatelliteHost>,
    focused: Option<&ResourceId>,
    panes: &HashMap<ResourceId, PaneSlot>,
) -> Option<String> {
    let focused = focused.filter(|id| id.host() == host)?;
    panes.get(focused)?.cwd.clone()
}

/// What the "Listing ..." placeholder names: the path (`~` for home), and
/// the satellite when the listing is relayed to one.
fn placeholder_label(host: &ListingHost, path: &str) -> String {
    let shown = if path.is_empty() { "~" } else { path };
    host.satellite()
        .map_or_else(|| shown.to_owned(), |host| format!("{shown} on {host}"))
}
