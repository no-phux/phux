//! A sample menu for previews, benchmarks and tests.

use super::*;

/// A sample menu with a header page and a long list, for previews and tests.
#[cfg(any(test, feature = "ui-fixtures"))]
pub(super) fn sample(skin: Skin, hosts: usize) -> MenuSpec {
    let noop = || -> Act { Rc::new(|_, _| {}) };
    let machines = MenuPage::titled(
        "Switch Machine",
        (0..hosts.max(1))
            .map(|index| {
                MenuItem::new(format!("Host {index}"), Choice::Run(noop()))
                    .icon(IconName::Server)
                    .detail("ssh://fixture.invalid")
            })
            .collect(),
    );
    MenuSpec {
        name: "Fixture menu".into(),
        root: MenuPage {
            heading: Some(Heading {
                title: "main".into(),
                detail: Some("This Mac".into()),
                icon: Some(IconName::LaptopMinimal),
                page: Some(Rc::new(machines)),
            }),
            items: vec![
                MenuItem::new("New Terminal", Choice::Run(noop())).icon(IconName::SquareTerminal),
                MenuItem::new("New Agent…", Choice::Run(noop())).icon(IconName::Bot),
                MenuItem::page(
                    "View",
                    MenuPage::titled(
                        "View",
                        vec![MenuItem::new("Tabs on Top", Choice::Run(noop())).checked(true)],
                    ),
                )
                .icon(IconName::PanelLeft)
                .gap(),
                MenuItem::new("Settings…", Choice::Run(noop()))
                    .icon(IconName::Settings)
                    .chord(Keystroke::parse("cmd-,").ok())
                    .selector("settings"),
            ],
        },
        width: 280.,
        skin,
        on_close: None,
    }
}

/// An ownerless menu in `window`, for previews and tests.
#[cfg(any(test, feature = "ui-fixtures"))]
pub(super) fn fixture(
    spec: MenuSpec,
    max_height: f32,
    window: &mut Window,
    cx: &mut App,
) -> Entity<GlassMenu> {
    cx.new(|cx| GlassMenu::new(spec, None, max_height, window, cx))
}

/// Standalone presentation of the production component.
#[cfg(feature = "ui-fixtures")]
pub(crate) fn preview(cx: &mut App) -> anyhow::Result<()> {
    // `PHUX_GLASS_PREVIEW_SKIN=terminal` previews the opaque skin.
    let skin =
        *crate::settings::skin_by_id(&std::env::var("PHUX_GLASS_PREVIEW_SKIN").unwrap_or_default());
    let spec = sample(skin, 2);
    let height = page_height(&spec.root, true);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(spec.width), px(height)), cx)),
            kind: WindowKind::PopUp,
            titlebar: None,
            focus: false,
            show: true,
            is_resizable: false,
            is_movable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title(&format!("{} menu", skin.name));
            fixture(spec, 700., window, cx)
        },
    )?;
    Ok(())
}

/// Synchronous creation cost with a warmed GPUI window; no visible windows or daemon.
#[cfg(feature = "ui-fixtures")]
pub(crate) fn benchmark(cx: &mut App) -> anyhow::Result<()> {
    use crate::settings::skin::GLASS;
    let options = || WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(20.), px(60.)),
            size(px(280.), px(400.)),
        ))),
        titlebar: None,
        focus: false,
        show: false,
        kind: WindowKind::PopUp,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    // Match the production path: GPUI already owns a window and a Metal context.
    let primer = cx.open_window(options(), |_, cx| cx.new(|_| BenchmarkPrimer))?;
    let mut samples = Vec::new();
    for _ in 0..12 {
        let started = Instant::now();
        let menu = cx.open_window(options(), |window, cx| {
            fixture(sample(GLASS, 0), 700., window, cx)
        })?;
        samples.push(started.elapsed().as_micros());
        menu.update(cx, |_, window, _| window.remove_window())?;
    }
    primer.update(cx, |_, window, _| window.remove_window())?;
    let first = samples[0];
    let mut warm = samples[1..].to_vec();
    warm.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"metric":"menu_creation_us", "first":first,"warm_median":warm[warm.len()/2], "warm_max":warm.last(), "samples":samples, "scope":"hidden native window creation including initial GPUI draw; excludes compositor presentation"})
    );
    Ok(())
}

#[cfg(feature = "ui-fixtures")]
struct BenchmarkPrimer;

#[cfg(feature = "ui-fixtures")]
impl Render for BenchmarkPrimer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("Menu performance fixture")
    }
}
