//! Capture every Settings page, and the Updates page in each of its states,
//! from a real desk window. Native scene captures only, for design review.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, AppContext as _};

use super::Page;
use crate::app::desk::Desk;
use crate::app::updates::Staged;
use crate::updates::{Release, RuntimeRelease};

type Scenario = (
    &'static str,
    Option<Page>,
    fn(&mut crate::app::updates::Updater),
);

fn release(version: &str) -> Release {
    Release {
        version: version.into(),
        revision: "0f3c9a1e5b7d2c4e6f8a0b1c3d5e7f9a1b3c5d7e".into(),
        protocol: crate::updates::own_protocol(),
        ..Release::default()
    }
}

fn runtime(available: bool) -> RuntimeRelease {
    RuntimeRelease {
        current: "0.41.2".into(),
        target: if available { "0.43.0" } else { "0.41.2" }.into(),
        available,
        mutable: true,
        native_command: None,
        protocol_current: None,
        protocol_target: None,
    }
}

const SCENARIOS: &[Scenario] = &[
    ("index", None, |_| {}),
    ("appearance", Some(Page::Appearance), |_| {}),
    ("layout", Some(Page::Layout), |_| {}),
    ("keyboard", Some(Page::Keyboard), |_| {}),
    ("spacing", Some(Page::Spacing), |_| {}),
    ("ghostty", Some(Page::Ghostty), |_| {}),
    ("connections", Some(Page::Connections), |_| {}),
    ("developer", Some(Page::Developer), |_| {}),
    ("updates-development", Some(Page::Updates), |_| {}),
    ("updates-current", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.43.0"));
        updater.target = Some(release("0.43.0"));
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(false));
        updater.message = "You’re up to date".into();
    }),
    ("updates-available", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.42.1"));
        updater.target = Some(release("0.43.0"));
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(true));
        updater.message = "An update is ready".into();
    }),
    ("updates-staged", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.42.1"));
        updater.target = Some(release("0.43.0"));
        updater.staged = Some(Staged {
            app: "/Applications/.phux-desktop-update/Phux.app".into(),
            release: release("0.43.0"),
        });
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(true));
    }),
    ("updates-busy", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.42.1"));
        updater.target = Some(release("0.43.0"));
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(true));
        updater.busy = true;
        updater.work = crate::app::updates::Work::Downloading;
        updater.message = "Downloading and verifying desktop update…".into();
    }),
    ("updates-phux", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.43.0"));
        updater.target = Some(release("0.43.0"));
        updater.prefs.manage_runtime = true;
        updater.runtime = Some(runtime(true));
    }),
    ("updates-failed", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.42.1"));
        updater.prefs.manage_runtime = true;
        updater.message = "Desktop: Could not resolve host: releases.phux.sh\nphux: release check timed out. Nothing was installed; try Check now again.".into();
    }),
    ("updates-unmanaged", Some(Page::Updates), |updater| {
        updater.development = false;
        updater.current = Some(release("0.43.0"));
        updater.target = Some(release("0.43.0"));
        updater.prefs.manage_runtime = false;
        updater.message = "You’re up to date".into();
    }),
];

/// Show each scenario in `root`'s desk and write `<name>.png` into `directory`.
pub(crate) fn capture(
    root: gpui::WindowHandle<gpui_kit::component::Root>,
    directory: PathBuf,
    cx: &mut App,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_secs(2)).await;
        for (name, page, arrange) in SCENARIOS {
            root.update(cx, |root, window, cx| {
                let desk = root.view().clone().downcast::<Desk>().expect("desk");
                desk.update(cx, |desk, cx| {
                    desk.updates.update(cx, |updater, cx| {
                        *updater = crate::app::updates::Updater::new();
                        arrange(updater);
                        cx.notify();
                    });
                    match page {
                        Some(page) => desk.show_settings_page(*page, window, cx),
                        None => desk.open_modal(crate::app::desk::Modal::Settings, window, cx),
                    }
                });
                window.refresh();
            })
            .expect("desk window");
            cx.background_executor()
                .timer(Duration::from_millis(700))
                .await;
            cx.update_window(*root, |_, window, cx| window.draw(cx).clear(cx))
                .expect("draw settings scene");
            root.update(cx, |_, window, _| {
                window
                    .render_to_image()
                    .expect("settings scene capture")
                    .save(directory.join(format!("{name}.png")))
                    .expect("write settings PNG");
            })
            .expect("capture window");
        }
        cx.update(|cx| cx.quit());
    })
    .detach();
    Ok(())
}
