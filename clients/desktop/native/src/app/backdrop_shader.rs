//! The window's backdrop shader: a WGSL program drawn behind the chrome and
//! panes, in the theme's colors, moving with the fleet.
//!
//! The choice is a display preference (`backdropShader`). A built-in compiles
//! once; a file in the shader folder is read when chosen and again whenever
//! it changes on disk, so saving it in an editor redraws the window. A
//! program that does not compile posts its error once and draws nothing.
//! The quick terminal and Plain terminal never draw one.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Task, div};

use crate::connection::{AgentInfo, AgentState};
use crate::settings::theme::Palette;
use crate::shader::library::{self, Choice, Loaded};
use crate::shader::{Program, Uniforms};

use super::color::from_hex;
use super::desk::{Desk, Paint, WindowRole};
use super::notifications::Kind;
use super::shader_view::{Fleet, Pace, ShaderView};

/// Soft, full-window effects: one pixel per point is plenty, and the window
/// behind a key window rarely needs more than a few frames a second.
const PACE: Pace = Pace {
    fps: 30.,
    background_fps: 6.,
    density: 1.,
    radius: 0.,
};

/// How often a chosen file is checked for a save.
const WATCH: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(crate) struct BackdropShader {
    choice: Option<Choice>,
    loaded: Option<Loaded>,
    program: Option<Arc<Program>>,
    view: Option<Entity<ShaderView>>,
    watch: Option<Task<()>>,
    /// The error last posted, so a file saved broken twice posts once.
    reported: Option<String>,
    /// The theme block for the paint it was built from; a terminal frame
    /// redraws the desk without changing either.
    theme: Option<(Rc<Paint>, Uniforms)>,
}

impl BackdropShader {
    /// The live view, for captures and tests.
    #[cfg(any(test, feature = "ui-fixtures"))]
    pub(crate) fn view(&self) -> Option<&Entity<ShaderView>> {
        self.view.as_ref()
    }

    /// Why the chosen shader draws nothing: its compile error or the GPU's.
    pub(crate) fn problem(&self, cx: &App) -> Option<String> {
        if let Some(Loaded {
            program: Err(error),
            ..
        }) = &self.loaded
        {
            return Some(error.to_string());
        }
        self.view
            .as_ref()
            .and_then(|view| view.read(cx).failure().map(str::to_owned))
    }
}

/// The theme's colors, and how strongly the shader shows, as the `phux` block.
#[must_use]
pub(crate) fn theme_uniforms(palette: &Palette, intensity: f32) -> Uniforms {
    let theme = &palette.theme;
    let color = |hex: &str| {
        let rgba = gpui::Rgba::from(from_hex(hex));
        [rgba.r, rgba.g, rgba.b, rgba.a]
    };
    let mut ansi = DEFAULT_ANSI.map(&color);
    for (slot, hex) in ansi.iter_mut().zip(theme.palette.iter().flatten()) {
        *slot = color(hex);
    }
    Uniforms {
        intensity: intensity.clamp(0., 1.),
        dark: if theme.appearance == "light" { 0. } else { 1. },
        background: color(&theme.background),
        foreground: color(&theme.foreground),
        surface: color(&theme.surface),
        accent: color(&theme.accent),
        cursor: color(&theme.cursor),
        selection: color(&theme.selection),
        danger: color(&theme.danger),
        warning: color(&theme.warning),
        success: color(&theme.success),
        ansi,
        ..Uniforms::default()
    }
}

/// For a theme that keeps the terminal's own palette.
const DEFAULT_ANSI: [&str; 16] = [
    "#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
    "#666666", "#d54e53", "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
];

/// Agent counts for the shader.
#[must_use]
pub(crate) fn fleet_of<'a>(agents: impl IntoIterator<Item = &'a AgentInfo>) -> Fleet {
    let mut fleet = Fleet::default();
    for info in agents {
        fleet.agents += 1;
        match info.state {
            AgentState::Working => fleet.working += 1,
            AgentState::Blocked => fleet.blocked += 1,
            AgentState::Done => fleet.done += 1,
            AgentState::Idle => fleet.idle += 1,
            AgentState::Unknown => {}
        }
        if info.needs_you() {
            fleet.needs_you += 1;
        }
    }
    fleet
}

/// What every shader view in a window is fed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Inputs {
    pub uniforms: Uniforms,
    pub fleet: Fleet,
    pub motion: bool,
}

impl Desk {
    /// The theme, fleet and motion this window's shaders see, at `intensity`.
    pub(crate) fn shader_inputs(&self, intensity: f32, cx: &App) -> Inputs {
        let agents = self.agent_catalog();
        Inputs {
            uniforms: theme_uniforms(&self.palette(), intensity),
            fleet: fleet_of(agents.iter().map(|agent| &agent.info)),
            motion: self.prefs.animations && !cx.reduce_motion(),
        }
    }

    /// The backdrop, kept in step with the preferences; `None` while off.
    pub(crate) fn backdrop_layer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let chrome_allows = self.role != WindowRole::Quick
            && !self.prefs.plain
            && self.prefs.skin_id != crate::settings::skin::TERMINAL.id;
        let choice = if chrome_allows {
            Choice::parse(&self.prefs.backdrop_shader)
        } else {
            Choice::Off
        };
        if self.backdrop_shader.choice.as_ref() != Some(&choice) {
            self.choose_backdrop(choice, cx);
        }
        let Some(program) = self.backdrop_shader.program.clone() else {
            self.backdrop_shader.view = None;
            return None;
        };
        let paint = self.paint();
        let uniforms = match &self.backdrop_shader.theme {
            Some((painted, uniforms)) if Rc::ptr_eq(painted, &paint) => *uniforms,
            _ => {
                let uniforms = theme_uniforms(&paint.palette, self.prefs.backdrop_intensity as f32);
                self.backdrop_shader.theme = Some((paint, uniforms));
                uniforms
            }
        };
        let agents = self.agent_catalog();
        let inputs = Inputs {
            uniforms,
            fleet: fleet_of(agents.iter().map(|agent| &agent.info)),
            motion: self.prefs.animations && !cx.reduce_motion(),
        };
        let view = self
            .backdrop_shader
            .view
            .get_or_insert_with(|| cx.new(|cx| ShaderView::new(PACE, cx)))
            .clone();
        view.update(cx, |view, cx| {
            view.set_program(Some(program), cx);
            view.set_uniforms(inputs.uniforms, cx);
            view.set_fleet(inputs.fleet, cx);
            view.set_motion(inputs.motion, cx);
        });
        Some(
            div()
                .id("backdrop-shader")
                .debug_selector(|| "backdrop-shader".into())
                .absolute()
                .inset_0()
                .child(view)
                .into_any_element(),
        )
    }

    /// Step to the next backdrop: off, the built-ins, then the shader folder.
    pub(crate) fn cycle_backdrop(&mut self, cx: &mut Context<Self>) {
        let mut choices = vec![Choice::Off];
        choices.extend(library::BUILTINS.iter().map(Choice::Builtin));
        choices.extend(
            library::files_in(&library::folder())
                .into_iter()
                .map(Choice::File),
        );
        let current = Choice::parse(&self.prefs.backdrop_shader);
        let next = choices
            .iter()
            .position(|choice| *choice == current)
            .map_or(1, |at| (at + 1) % choices.len());
        let next = choices[next].id();
        self.update_prefs(|prefs| prefs.backdrop_shader = next, cx);
    }

    fn choose_backdrop(&mut self, choice: Choice, cx: &mut Context<Self>) {
        self.backdrop_shader.watch = matches!(choice, Choice::File(_)).then(|| {
            cx.spawn(async move |desk, cx| {
                loop {
                    cx.background_executor().timer(WATCH).await;
                    let watching = desk
                        .update(cx, |desk, cx| desk.reload_backdrop_if_saved(cx))
                        .unwrap_or(false);
                    if !watching {
                        break;
                    }
                }
            })
        });
        self.backdrop_shader.choice = Some(choice);
        self.backdrop_shader.reported = None;
        self.load_backdrop(cx);
    }

    fn load_backdrop(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.backdrop_shader;
        state.loaded = state.choice.as_ref().and_then(library::load);
        state.program = match &state.loaded {
            Some(Loaded {
                program: Ok(program),
                ..
            }) => Some(Arc::new(program.clone())),
            _ => None,
        };
        let failure = match &state.loaded {
            Some(Loaded {
                program: Err(error),
                ..
            }) => Some(error.to_string()),
            _ => None,
        };
        if failure.is_some() && failure != state.reported {
            let label = state.choice.as_ref().map(Choice::label).unwrap_or_default();
            state.reported.clone_from(&failure);
            self.post(
                Kind::Error,
                "Backdrop shader did not compile",
                failure.map(|error| format!("{label}: {error}")),
            );
        } else if failure.is_none() {
            state.reported = None;
        }
        cx.notify();
    }

    /// Reload the chosen file when it was saved since it was read. False once
    /// the choice is no longer a file, which ends the watch.
    fn reload_backdrop_if_saved(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.backdrop_shader.choice.as_ref().and_then(Choice::path) else {
            return false;
        };
        let read = self
            .backdrop_shader
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.modified);
        if library::modified(&path) != read {
            self.load_backdrop(cx);
        }
        true
    }
}

#[cfg(test)]
mod tests;
