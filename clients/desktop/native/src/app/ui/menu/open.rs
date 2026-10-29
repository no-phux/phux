//! Opening a menu in its own popup window.

use super::*;

/// Open `spec` from `anchor` in `window`. The menu follows `owner`'s skin
/// through `skin_of` and closes when the owner goes, the parent moves or
/// hides, or the app deactivates.
pub(crate) fn open<T: 'static>(
    spec: MenuSpec,
    anchor: Anchor,
    owner: &Entity<T>,
    skin_of: fn(&T) -> Skin,
    window: &mut Window,
    cx: &mut App,
) -> Option<WindowHandle<GlassMenu>> {
    let parent = window.window_handle();
    let return_focus = window.focused(cx);
    let bounds = window.bounds();
    let display = window.display(cx);
    let display_id = display.as_ref().map(|display| display.id());
    let screen = display.map(|display| display.bounds()).unwrap_or(bounds);
    let width = spec.width;
    let wanted = page_height(&spec.root, true);
    let (origin, max_height) = place(anchor, bounds, screen, width, wanted);
    let height = wanted.min(max_height);
    // The platform may refuse a window kind before building its view; the
    // spec waits in the cell for the next attempt.
    let spec = Rc::new(std::cell::RefCell::new(Some(spec)));
    let build = |kind: WindowKind, cx: &mut App| {
        let (spec, return_focus) = (spec.clone(), return_focus.clone());
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    origin,
                    size(
                        px(width),
                        px(height - if cx.reduce_motion() { 0. } else { 14. }).max(px(ROW)),
                    ),
                ))),
                display_id,
                titlebar: None,
                focus: crate::diag::may_activate(),
                show: true,
                kind,
                is_resizable: false,
                is_movable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            move |window, cx| {
                let spec = spec
                    .borrow_mut()
                    .take()
                    .expect("a menu window is built once");
                cx.new(|cx| {
                    GlassMenu::new(spec, Some((parent, return_focus)), max_height, window, cx)
                })
            },
        )
    };
    // A compositor that owns placement (Wayland) takes a parent-anchored
    // popup; the rest reject it and take an absolutely placed one.
    let (edge, gravity, drop) = match anchor {
        Anchor::Above(_) => (
            gpui::popup::PopupAnchor::TopLeft,
            gpui::popup::PopupGravity::TopRight,
            -DROP,
        ),
        _ => (
            gpui::popup::PopupAnchor::BottomLeft,
            gpui::popup::PopupGravity::BottomRight,
            DROP,
        ),
    };
    let anchored = WindowKind::AnchoredPopup(gpui::popup::PopupOptions {
        parent,
        anchor_rect: anchor.rect(),
        anchor: edge,
        gravity,
        constraint_adjustment: gpui::popup::PopupConstraintAdjustment::SLIDE_X
            | gpui::popup::PopupConstraintAdjustment::SLIDE_Y
            | gpui::popup::PopupConstraintAdjustment::FLIP_Y,
        offset: point(px(0.), px(drop)),
        grab: false,
    });
    let handle = match build(anchored, cx).or_else(|_| build(WindowKind::PopUp, cx)) {
        Ok(handle) => handle,
        Err(error) => {
            log::error!("opening a menu: {error:#}");
            return None;
        }
    };
    let close = move |cx: &mut App| {
        let _ = AnyWindowHandle::from(handle).update(cx, |_, window, _| window.remove_window());
    };
    let entity = handle.entity(cx).ok()?;
    entity.update(cx, |menu, cx| {
        menu._subscriptions
            .push(cx.observe_window_bounds(window, move |menu, parent, cx| {
                // GPUI also reports bounds on activation. Opening this panel
                // deactivates its parent without moving it.
                if parent.bounds() == bounds {
                    return;
                }
                menu.finish(cx);
                close(cx);
            }));
        menu._subscriptions.push(cx.observe_window_visibility(
            window,
            move |menu, visibility, _, cx| {
                if !visibility.is_visible() {
                    menu.finish(cx);
                    close(cx);
                }
            },
        ));
        // The owner notifies on every frame of a skin shift; follow it.
        menu._subscriptions
            .push(cx.observe(owner, move |menu, owner, cx| {
                let skin = skin_of(owner.read(cx));
                if skin != menu.skin {
                    menu.skin = skin;
                    cx.notify();
                }
            }));
        menu._subscriptions
            .push(cx.observe_release(owner, move |menu, _, cx| {
                menu.finish(cx);
                close(cx);
            }));
    });
    Some(handle)
}
