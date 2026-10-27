//! Server-free captures of the desk wearing each built-in backdrop shader,
//! and of the Settings gallery that picks one.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, AppContext as _, WindowHandle, px, size};
use gpui_kit::component::Root;

use super::{arrange_navigation, scene_preferences};
use crate::app::{AppOptions, Desk};

/// The navigation scene (a split, a working and a blocked agent) wearing each
/// built-in backdrop shader at its shipped intensity.
pub(crate) fn open_backdrops(
    options: AppOptions,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.set_reduce_motion(true);
    let root = crate::app::open(options, cx)?;
    cx.spawn(async move |cx| {
        let scenes = ["midnight", "latte"].into_iter().flat_map(|theme| {
            [0.92, 0.6].into_iter().flat_map(move |opacity| {
                crate::shader::library::BUILTINS
                    .iter()
                    .map(move |builtin| (theme, opacity, builtin))
            })
        });
        {
            for (theme, opacity, builtin) in scenes {
                root.update(cx, |root, window, cx| {
                    let desk = root.view().clone().downcast::<Desk>().unwrap();
                    desk.update(cx, |desk, cx| {
                        arrange_navigation(desk, cx);
                        desk.update_prefs(
                            |prefs| {
                                scene_preferences(prefs, "backdrop", theme, true);
                                prefs.tab_position = crate::settings::TabPosition::Side;
                                prefs.backdrop_shader = builtin.id.into();
                                prefs.terminal_opacity = opacity;
                            },
                            cx,
                        );
                    });
                    window.resize(size(px(1280.), px(800.)));
                    window.refresh();
                })
                .expect("arrange backdrop");
                // A capture window draws only when asked: this draw hands the
                // view its program, and the wait covers the pipeline compile
                // and the frame's read-back.
                cx.update_window(root.into(), |_, window, cx| window.draw(cx).clear(cx))
                    .expect("hand over the program");
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                cx.update_window(root.into(), |_, window, cx| window.draw(cx).clear(cx))
                    .expect("draw backdrop");
                cx.update_window(root.into(), |_, window, _| {
                    window
                        .render_to_image()
                        .expect("native scene capture")
                        .save(directory.join(format!(
                            "{theme}-backdrop-{}-{:.0}.png",
                            builtin.id,
                            opacity * 100.
                        )))
                        .expect("write PNG");
                })
                .expect("capture backdrop");
            }
        }
        capture_backdrop_settings(root, &directory, cx).await;
        check_hidden_backdrop_rests(root, cx).await;
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}

/// With motion on, an animated backdrop runs its clock only while the
/// platform presents the window. Capture windows are never presented, so
/// after the first frame the clock must rest.
async fn check_hidden_backdrop_rests(root: WindowHandle<Root>, cx: &mut gpui::AsyncApp) {
    let desk = root
        .update(cx, |root, _, cx| {
            cx.set_reduce_motion(false);
            let desk = root.view().clone().downcast::<Desk>().unwrap();
            desk.update(cx, |desk, cx| {
                desk.update_prefs(
                    |prefs| {
                        prefs.animations = true;
                        prefs.backdrop_shader = "rain".into();
                    },
                    cx,
                );
            });
            desk
        })
        .expect("motion on");
    cx.update_window(root.into(), |_, window, cx| window.draw(cx).clear(cx))
        .expect("draw");
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
    let (visible, before) = root
        .update(cx, |_, window, cx| (window.is_visible(), frames(&desk, cx)))
        .expect("window");
    cx.background_executor().timer(Duration::from_secs(1)).await;
    let after = cx.update(|cx| frames(&desk, cx));
    eprintln!(
        "backdrop clock: {} frames in a second (visible: {visible})",
        after - before
    );
    assert!(visible || after == before, "a hidden window kept rendering");
}

fn frames(desk: &gpui::Entity<Desk>, cx: &App) -> u32 {
    desk.read(cx)
        .backdrop_shader
        .view()
        .map_or(0, |view| view.read(cx).frames())
}

/// Settings searched for "shader": the live gallery, the intensity and the
/// shader folder.
async fn capture_backdrop_settings(
    root: WindowHandle<Root>,
    directory: &std::path::Path,
    cx: &mut gpui::AsyncApp,
) {
    let desk = root
        .update(cx, |root, window, cx| {
            let desk = root.view().clone().downcast::<Desk>().unwrap();
            desk.update(cx, |desk, cx| {
                desk.update_prefs(
                    |prefs| {
                        scene_preferences(prefs, "backdrop", "midnight", true);
                        prefs.backdrop_shader = "aurora".into();
                    },
                    cx,
                );
                desk.execute("settings", window, cx);
            });
            desk
        })
        .expect("open settings");
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
    let (handle, settings) = cx
        .update(|cx| desk.read(cx).desktop.read(cx).settings.clone())
        .expect("settings window");
    cx.update_window(handle, |_, window, cx| {
        settings.update(cx, |settings, cx| settings.search("shader", window, cx));
        window.draw(cx).clear(cx);
    })
    .expect("search settings");
    cx.background_executor()
        .timer(Duration::from_millis(1200))
        .await;
    cx.update_window(handle, |_, window, cx| {
        window.draw(cx).clear(cx);
        window
            .render_to_image()
            .expect("settings capture")
            .save(directory.join("settings-backdrop.png"))
            .expect("write PNG");
    })
    .expect("capture settings");
}
