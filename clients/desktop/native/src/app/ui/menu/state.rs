//! The menu's state: pages, selection, keys and its surface motion.

use super::*;

impl GlassMenu {
    pub(super) fn new(
        spec: MenuSpec,
        parent: Option<(AnyWindowHandle, Option<FocusHandle>)>,
        max_height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A positive alpha keeps AppKit's drawable delivery alive during entry.
        window.set_window_opacity(0.01);
        let appearance = if cx.theme().is_dark() {
            WindowAppearance::Dark
        } else {
            WindowAppearance::Light
        };
        // Opaque skins paint their own solid surface; only clear ones get
        // the compositor material.
        let skin = spec.skin;
        let native_glass = wants_glass(&skin)
            && window.set_glass_effect(px(skin.shape.popover_radius), appearance);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let owned = parent.is_some();
        let (parent, return_focus) = parent.unzip();
        let root = Rc::new(spec.root);
        let height = page_height(&root, true);
        let mut menu = Self {
            name: spec.name,
            parent,
            return_focus: return_focus.flatten(),
            focus,
            stack: vec![root],
            selected: None,
            pointer: window.mouse_position(),
            scroll: ScrollHandle::new(),
            width: spec.width,
            max_height,
            page_generation: 0,
            opened: Instant::now(),
            closing: None,
            native_glass,
            skin,
            surface: SurfaceMotion::new(height.min(max_height)),
            chosen: false,
            on_close: spec.on_close,
            _subscriptions: Vec::new(),
        };
        if owned {
            menu._subscriptions
                .push(cx.observe_window_activation(window, |menu, window, cx| {
                    if !window.is_window_active() {
                        menu.dismiss(false, window, cx);
                    }
                }));
        }
        cx.on_release(|menu, cx| menu.finish(cx)).detach();
        menu
    }

    pub(super) fn page(&self) -> &MenuPage {
        self.stack.last().expect("a menu always has its first page")
    }

    pub(super) fn first(&self) -> bool {
        self.stack.len() == 1
    }

    /// Whether the header takes clicks and a keyboard slot.
    pub(super) fn header_acts(&self) -> bool {
        !self.first()
            || self
                .page()
                .heading
                .as_ref()
                .is_some_and(|heading| heading.page.is_some())
    }

    pub(super) fn header_height(&self) -> f32 {
        header_height(self.page(), self.first())
    }

    pub(super) fn show(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.scroll = ScrollHandle::new();
        self.page_generation += 1;
        self.surface.target = page_height(self.page(), self.first()).min(self.max_height);
        cx.notify();
    }

    pub(super) fn push(&mut self, page: Rc<MenuPage>, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.stack.push(page);
        self.show(cx);
    }

    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        if self.closing.is_some() || self.first() {
            return;
        }
        self.stack.pop();
        self.show(cx);
    }

    /// Run the close callback, once.
    pub(super) fn finish(&mut self, cx: &mut App) {
        if let Some(on_close) = self.on_close.take() {
            on_close(
                if self.chosen {
                    Closed::Chosen
                } else {
                    Closed::Away
                },
                cx,
            );
        }
    }

    pub(crate) fn dismiss(&mut self, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        self.closing = Some(Instant::now());
        self.finish(cx);
        // Occluded panels do not receive display frames. Cleanup must still
        // finish when the user switches Spaces or the app deactivates.
        let handle = window.window_handle();
        let exit = self.skin.motion.menu_exit();
        if cx.reduce_motion() {
            window.remove_window();
        } else {
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(exit).await;
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            })
            .detach();
        }
        if let Some(parent) = self.parent.filter(|_| restore) {
            let focus = self.return_focus.clone();
            let _ = parent.update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(focus) = focus {
                    window.focus(&focus, cx);
                }
            });
        }
        cx.notify();
    }

    /// Whether the menu is on its way out.
    pub(crate) fn closing(&self) -> bool {
        self.closing.is_some()
    }

    pub(super) fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        if self.closing.is_some() {
            return;
        }
        let act = match choice {
            Choice::Page(page) => return self.push(page, cx),
            Choice::Run(act) => act,
        };
        self.chosen = true;
        self.dismiss(true, window, cx);
        if let Some(parent) = self.parent {
            let _ = parent.update(cx, move |_, window, cx| act(window, cx));
        }
    }

    /// The header opens its page from the first page and goes back from any other.
    pub(super) fn header_click(&mut self, cx: &mut Context<Self>) {
        if !self.first() {
            return self.back(cx);
        }
        if let Some(page) = self
            .page()
            .heading
            .as_ref()
            .and_then(|heading| heading.page.clone())
        {
            self.push(page, cx);
        }
    }

    pub(super) fn move_selection(&mut self, up: bool, window: &Window, cx: &mut Context<Self>) {
        // With an acting header, slot zero is the header, so keyboard users
        // can always switch pages.
        let header = usize::from(self.header_acts());
        let count = self.page().items.len() + header;
        if count == 0 {
            return;
        }
        let slot = self.selected.map_or(0, |index| index + header);
        let next = match (self.selected, header) {
            (None, 0) if up => count - 1,
            (None, 0) => 0,
            _ if up => (slot + count - 1) % count,
            _ => (slot + 1) % count,
        };
        self.selected = next.checked_sub(header);
        cx.notify();
        let Some(index) = self.selected else {
            return;
        };
        let page = self.page();
        let top = page.row_y(index);
        let bottom = top + page.items[index].height();
        let height = window.viewport_size().height.as_f32() - self.header_height() - INSET * 2.;
        let mut offset = self.scroll.offset();
        if top < -offset.y.as_f32() {
            offset.y = px(-top);
        } else if bottom > height - offset.y.as_f32() {
            offset.y = px(height - bottom);
        }
        self.scroll.set_offset(offset);
        cx.notify();
    }

    pub(super) fn activate_selection(
        &mut self,
        pages_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.selected else {
            if self.header_acts() && !(pages_only && !self.first()) {
                self.header_click(cx);
            }
            return;
        };
        let choice = self.page().items[index].choice.clone();
        if !pages_only || matches!(choice, Choice::Page(_)) {
            self.choose(choice, window, cx);
        }
    }

    /// An item, on any page, whose chord is `stroke`.
    pub(super) fn chorded(&self, stroke: &Keystroke) -> Option<Choice> {
        fn find(page: &MenuPage, stroke: &Keystroke) -> Option<Choice> {
            page.items.iter().find_map(|item| match &item.choice {
                _ if item.chord.as_ref() == Some(stroke) => Some(item.choice.clone()),
                Choice::Page(page) => find(page, stroke),
                Choice::Run(_) => None,
            })
        }
        find(&self.stack[0], stroke)
    }

    pub(super) fn hover_select(
        &mut self,
        selected: Option<usize>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.closing.is_some() || self.pointer == position {
            return;
        }
        self.pointer = position;
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    pub(super) fn key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        if self.closing.is_some() {
            return;
        }
        self.pointer = window.mouse_position();
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss(true, window, cx),
            "left" => self.back(cx),
            "up" | "down" => self.move_selection(event.keystroke.key == "up", window, cx),
            "enter" | "space" => self.activate_selection(false, window, cx),
            "right" => self.activate_selection(true, window, cx),
            _ => {
                if let Some(choice) = self.chorded(&event.keystroke) {
                    self.choose(choice, window, cx);
                }
            }
        }
    }

    /// An app action that reached the menu (a menu-bar command while the
    /// menu has focus) closes it and runs in the parent window.
    pub(super) fn forward(
        &mut self,
        action: &crate::app::actions::Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = action.clone();
        self.choose(
            Choice::Run(Rc::new(move |window, cx| {
                window.dispatch_action(Box::new(action.clone()), cx)
            })),
            window,
            cx,
        );
    }

    pub(super) fn opacity(&self, now: Instant, reduced: bool) -> (f32, bool) {
        if reduced {
            return (if self.closing.is_some() { 0. } else { 1. }, false);
        }
        let motion = &self.skin.motion;
        if let Some(at) = self.closing {
            let presence = 1. - span_progress(now.duration_since(at), motion.menu_exit());
            return (presence * presence, presence > 0.);
        }
        let progress = span_progress(now.duration_since(self.opened), motion.menu_enter());
        (1. - (1. - progress).powi(3), progress < 1.)
    }

    /// Called only during render, where GPUI permits animation frame requests.
    pub(super) fn animate_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let reduced = cx.reduce_motion();
        let moving = self.surface.advance(now, reduced, spring(&self.skin));
        let height = self.surface.height.position;
        if (window.viewport_size().height.as_f32() - height).abs() > 0.5 {
            window.resize(size(px(self.width), px(height)));
        }
        let (alpha, fading) = self.opacity(now, reduced);
        if self.closing.is_some() && alpha <= 0. {
            window.remove_window();
            return;
        }
        window.set_window_opacity(alpha.max(0.01));
        if !reduced && (moving || fading) {
            window.request_animation_frame();
        }
    }
}
