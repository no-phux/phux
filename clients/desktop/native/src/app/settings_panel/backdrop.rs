//! Settings > Appearance > Backdrop shader: a live card for every choice,
//! drawn in the window's own theme and fed its fleet, then the intensity and
//! the shader folder.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::prelude::*;
use gpui::{AnyElement, App, Entity, SharedString, WeakEntity, div, px};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{IconName, Sizable};

use crate::app::backdrop_shader::Inputs;
use crate::app::color::from_hex;
use crate::app::desk::Desk;
use crate::app::notifications::Kind;
use crate::app::shader_view::{Pace, ShaderView};
use crate::shader::library::{self, Choice, Loaded};

use super::catalog::{Group as SettingGroup, Item as SettingItem};
use super::{Knob, SettingsControls, click_sets, look_of, prefs_of, set_prefs, slider_item};

const PREVIEW: Pace = Pace {
    fps: 20.,
    background_fps: 2.,
    density: 2.,
    radius: 7.,
};
const CARD_WIDTH: f32 = 128.;
const CARD_HEIGHT: f32 = 76.;
/// How often an open Settings looks for new or saved shader files.
const SCAN: Duration = Duration::from_secs(1);

#[derive(Clone)]
struct Card {
    choice: Choice,
    view: Option<Entity<ShaderView>>,
    error: Option<String>,
    modified: Option<SystemTime>,
}

/// The cards' live views, kept with the Settings controls.
#[derive(Default)]
pub(crate) struct Gallery {
    cards: Vec<Card>,
    scanned: Option<Instant>,
}

impl Gallery {
    /// Follow the shader folder and feed every card the window's inputs.
    pub(super) fn sync(&mut self, inputs: Inputs, cx: &mut App) {
        if self.scanned.is_none_or(|at| at.elapsed() >= SCAN) {
            self.scanned = Some(Instant::now());
            self.rescan(cx);
        }
        let preview = Inputs {
            uniforms: crate::shader::Uniforms {
                intensity: 1.,
                ..inputs.uniforms
            },
            ..inputs
        };
        for view in self.cards.iter().filter_map(|card| card.view.as_ref()) {
            view.update(cx, |view, cx| {
                view.set_uniforms(preview.uniforms, cx);
                view.set_fleet(preview.fleet, cx);
                view.set_motion(preview.motion, cx);
            });
        }
    }

    fn rescan(&mut self, cx: &mut App) {
        let folder = library::folder();
        let choices = std::iter::once(Choice::Off)
            .chain(library::BUILTINS.iter().map(Choice::Builtin))
            .chain(library::files_in(&folder).into_iter().map(Choice::File));
        let mut previous = std::mem::take(&mut self.cards);
        for choice in choices {
            let kept = previous
                .iter()
                .position(|card| card.choice == choice)
                .map(|at| previous.swap_remove(at));
            let modified = choice.path().and_then(|path| library::modified(&path));
            let card = match kept {
                Some(card) if card.modified == modified => card,
                kept => compile(choice, modified, kept.and_then(|card| card.view), cx),
            };
            self.cards.push(card);
        }
    }
}

fn compile(
    choice: Choice,
    modified: Option<SystemTime>,
    view: Option<Entity<ShaderView>>,
    cx: &mut App,
) -> Card {
    let Some(Loaded { program, .. }) = library::load(&choice) else {
        return Card {
            choice,
            view: None,
            error: None,
            modified,
        };
    };
    let (program, error) = match program {
        Ok(program) => (Some(Arc::new(program)), None),
        Err(error) => (None, Some(error.to_string())),
    };
    let view = view.unwrap_or_else(|| cx.new(|cx| ShaderView::new(PREVIEW, cx)));
    view.update(cx, |view, cx| view.set_program(program, cx));
    Card {
        choice,
        view: Some(view),
        error,
        modified,
    }
}

/// The cards, the intensity and the folder buttons.
pub(super) fn group(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    controls: Option<&SettingsControls>,
    cx: &App,
) -> SettingGroup {
    let cards = controls
        .map(|controls| controls.gallery.cards.clone())
        .unwrap_or_default();
    let reader = handle.clone();
    let gallery = SettingItem::render("Backdrop shader", move |_, _, cx| {
        gallery(&cards, &reader, cx)
    })
    .description(status(desk, cx))
    .keywords([
        "shader",
        "wgsl",
        "backdrop",
        "background",
        "wallpaper",
        "animated",
        "aurora",
        "nebula",
        "rain",
        "grid",
        "silk",
        "glsl",
        "effects",
    ]);
    let folder = handle.clone();
    let files = SettingItem::new(
        "Your shaders",
        gpui_kit::component::setting::SettingField::render(move |_, _, _| {
            folder_buttons(folder.clone())
        }),
    )
    .description(format!(
        "Each .wgsl file in {} is a backdrop. New Shader writes a commented starter there; saving a file redraws the window.",
        crate::devtools::display_path(&library::folder())
    ))
    .keywords(["shader", "wgsl", "folder", "custom", "write", "starter"]);
    SettingGroup::new()
        .title("Backdrop shader")
        .item(gallery)
        .items(slider_item(
            controls,
            handle,
            Knob::BACKDROP,
            "How strongly the shader shows through the glass and panes; terminal text stays opaque.",
        ))
        .item(files)
}

/// What the chosen backdrop is doing, or why it is not.
fn status(desk: &Desk, cx: &App) -> String {
    let choice = Choice::parse(&desk.prefs.backdrop_shader);
    if let Some(problem) = desk.backdrop_shader.problem(cx) {
        return format!("{}: {problem}", choice.label());
    }
    let hidden = if desk.prefs.plain {
        " Plain terminal hides it."
    } else {
        ""
    };
    match choice {
        Choice::Off => "A WGSL shader behind the chrome and panes, in your theme's colors. Busy agents speed it up; one that needs you makes it flare.".to_owned(),
        Choice::Builtin(builtin) => format!("{}{hidden}", builtin.blurb),
        Choice::File(name) => format!("{name} redraws when you save it.{hidden}"),
    }
}

fn gallery(cards: &[Card], handle: &WeakEntity<Desk>, cx: &App) -> AnyElement {
    let picked = Choice::parse(&prefs_of(handle, cx).backdrop_shader);
    let note = handle.upgrade().map(|desk| status(desk.read(cx), cx));
    let look = look_of(handle, cx);
    let theme = &look.chrome.theme;
    let accent = from_hex(&theme.accent);
    let sheet = from_hex(&theme.background);
    let subtext = from_hex(&look.chrome.subtext);
    let card = |card: &Card| {
        let selected = card.choice == picked;
        let id = card.choice.id();
        let face = div()
            .relative()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .rounded(px(PREVIEW.radius))
            .overflow_hidden()
            .bg(sheet)
            .border_1()
            .border_color(if selected { accent } else { look.border })
            .when(selected, |face| {
                face.shadow(vec![crate::app::ui::shadow(
                    accent.opacity(0.35),
                    0.,
                    10.,
                    0.,
                    false,
                )])
            })
            .children(
                card.view
                    .clone()
                    .map(|view| div().absolute().inset_0().child(view)),
            )
            .children(card.error.as_ref().map(|error| {
                div()
                    .absolute()
                    .inset_0()
                    .p(px(8.))
                    .text_xs()
                    .text_color(from_hex(&theme.danger))
                    .child(SharedString::from(error.clone()))
            }));
        div()
            .id(SharedString::from(format!("backdrop-card-{id}")))
            .debug_selector(move || format!("backdrop-card-{id}"))
            .flex()
            .flex_col()
            .gap(px(6.))
            .cursor_pointer()
            .child(face)
            .child(
                div()
                    .text_sm()
                    .truncate()
                    .w(px(CARD_WIDTH))
                    .text_color(if selected { look.ink } else { subtext })
                    .child(card.choice.label()),
            )
            .on_click(click_sets(handle, {
                let value = card.choice.id();
                move |prefs| prefs.backdrop_shader.clone_from(&value)
            }))
    };
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.))
                .children(cards.iter().map(card)),
        )
        .children(note.map(|note| {
            div()
                .text_sm()
                .text_color(subtext)
                .child(SharedString::from(note))
        }))
        .into_any_element()
}

fn folder_buttons(handle: WeakEntity<Desk>) -> impl IntoElement {
    let (creator, opener) = (handle.clone(), handle);
    div()
        .flex()
        .gap(px(8.))
        .child(
            Button::new("backdrop-new-shader")
                .label("New Shader")
                .icon(IconName::Plus)
                .small()
                .outline()
                .on_click(move |_, _, cx| new_shader(&creator, cx)),
        )
        .child(
            Button::new("backdrop-open-folder")
                .label("Show Folder")
                .icon(IconName::FolderOpen)
                .small()
                .ghost()
                .on_click(move |_, _, cx| {
                    let folder = library::folder();
                    match std::fs::create_dir_all(&folder) {
                        Ok(()) => cx.open_with_system(&folder),
                        Err(error) => report(&opener, &folder, &error, cx),
                    }
                }),
        )
}

/// Write a starter, wear it, and show it in the file manager.
fn new_shader(handle: &WeakEntity<Desk>, cx: &mut App) {
    let folder = library::folder();
    match library::create_starter(&folder) {
        Ok(name) => {
            let path = folder.join(&name);
            set_prefs(handle, cx, |prefs| {
                prefs.backdrop_shader = Choice::File(name).id()
            });
            cx.reveal_path(&path);
        }
        Err(error) => report(handle, &folder, &error, cx),
    }
}

fn report(
    handle: &WeakEntity<Desk>,
    folder: &std::path::Path,
    error: &std::io::Error,
    cx: &mut App,
) {
    let body = format!("{}: {error}", folder.display());
    let _ = handle.update(cx, |desk, cx| {
        desk.post(Kind::Error, "Could not open the shader folder", Some(body));
        cx.notify();
    });
}
