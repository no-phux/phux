//! The colours the canvas paints with, and their CSS forms.
//!
//! The canvas chrome (backdrop, pane borders, search marks, the bell) takes
//! its roles from the pane's own terminal palette through the one solver,
//! `phux_design::derive`, so the chrome matches the terminal. A
//! pane with no palette of its own paints the house terminal palette, whose
//! roles are the authored house look. The DOM around the canvas reads the
//! same roles from `assets/phux-tokens.css` (`var(--phux-…)`).
//!
//! This is the crate's one style-literal owner: every colour reaches CSS
//! through [`css`] or [`var`], from a `phux_design` value.

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::Rc;

use phux_design::solve::{mode_of, over, with_alpha};
use phux_design::{Layer, Mode, Params, Rgb8, Rgba8, Roles, Theme, contrast_more, derive, tokens};
use phux_vt_web::{Grid, Rgb};

/// The bell wash's alpha over the canvas (`status.bell` at
/// α 0.18 for `curve.fade`).
pub const BELL_WASH_ALPHA: f64 = 0.18;

/// How many solved palettes stay cached: one per distinct pane palette on
/// screen is plenty (a canvas holds at most four panes).
const CACHE: usize = 4;

/// A palette's cache key: its default colours and whether the page asked
/// for more contrast.
type Key = (Rgb8, Rgb8, bool);

thread_local! {
    static SOLVED: RefCell<Vec<(Key, Rc<Roles>)>> = const { RefCell::new(Vec::new()) };
    static MORE_CONTRAST: Cell<bool> = const { Cell::new(false) };
}

/// Whether the canvas paints the increased-contrast variant: the
/// client sets it from `prefers-contrast: more` before each paint.
pub fn set_more_contrast(more: bool) {
    MORE_CONTRAST.with(|flag| flag.set(more));
}

const fn rgb8(c: Rgb) -> Rgb8 {
    Rgb8 {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

const fn vt_rgb(c: Rgba8) -> Rgb {
    Rgb {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

/// The house terminal palette for `mode` (catalog id `phux`/`phux-light`).
const fn house_terminal(mode: Mode) -> &'static tokens::TerminalPalette {
    match mode {
        Mode::Dark => &tokens::TERMINAL.dark,
        Mode::Light => &tokens::TERMINAL.light,
    }
}

fn house_ansi(mode: Mode) -> [Rgb8; 16] {
    let mut ansi = [Rgb8::BLACK; 16];
    for (slot, c) in ansi.iter_mut().zip(house_terminal(mode).ansi) {
        *slot = c.rgb();
    }
    ansi
}

/// Every role for a terminal whose default colours are `bg` and `fg`.
///
/// The house terminal palette gets the authored house look. Any other is
/// derived. The engine reports only the default colours, so the solver's
/// ANSI inputs are the house terminal's, in the mode the ground's
/// luminance picks. Under [`set_more_contrast`] the result is the
/// increased-contrast variant. Results are cached, so a paint never
/// re-solves.
#[must_use]
pub fn roles_for(bg: Rgb, fg: Rgb) -> Rc<Roles> {
    let key = (rgb8(bg), rgb8(fg), MORE_CONTRAST.with(Cell::get));
    SOLVED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, roles)) = cache.iter().find(|(k, _)| *k == key) {
            return Rc::clone(roles);
        }
        let roles = Rc::new(solve(key));
        if cache.len() == CACHE {
            cache.remove(0);
        }
        cache.push((key, Rc::clone(&roles)));
        roles
    })
}

fn solve((bg, fg, more): Key) -> Roles {
    for mode in [Mode::Dark, Mode::Light] {
        let house = house_terminal(mode);
        if house.bg.rgb() == bg && house.fg.rgb() == fg {
            return if more {
                Roles::house_contrast_more(mode)
            } else {
                Roles::house(mode)
            };
        }
    }
    let params = Params::house();
    let theme = Theme {
        bg,
        fg,
        ansi: house_ansi(Mode::Dark),
        cursor: None,
        accent: None,
        selection_bg: None,
        dark: None,
    };
    let theme = Theme {
        ansi: house_ansi(mode_of(&theme, &params)),
        ..theme
    };
    let roles = derive(&theme, &params);
    if more {
        contrast_more(&roles, &theme.ansi, &params)
    } else {
        roles
    }
}

/// The roles for `grid`'s own palette.
#[must_use]
pub fn roles_of(grid: &Grid) -> Rc<Roles> {
    roles_for(grid.default_bg, grid.default_fg)
}

/// Give a pane the house terminal palette when its defaults are only the
/// engine's own (`engine`: a fresh terminal's default background and
/// foreground), so a program that set no colours paints the house look.
/// The terminal client stays dark, as on the phone: the engine's ANSI
/// palette is tuned for a dark ground.
pub fn adopt_house_palette(grid: &mut Grid, engine: (Rgb, Rgb)) {
    if (grid.default_bg, grid.default_fg) == engine {
        let house = house_terminal(Mode::Dark);
        grid.default_bg = vt_rgb(house.bg);
        grid.default_fg = vt_rgb(house.fg);
    }
}

/// A search hit's background over a cell background `bg`: `match.bg` is
/// translucent, so the cell's own ink stays on it.
#[must_use]
pub fn match_fill(roles: &Roles, bg: Rgb) -> Rgb {
    let flat = over(roles.r#match.bg, rgb8(bg));
    Rgb {
        r: flat.r,
        g: flat.g,
        b: flat.b,
    }
}

/// The fills a search hit and the current hit paint over the house ground,
/// as RGB: what a pane with no palette of its own shows (`match.bg`
/// composited, then `match.current_bg`).
#[must_use]
pub fn house_match_pixels() -> [[u8; 3]; 2] {
    let roles = house();
    let hit = match_fill(&roles, vt_rgb(roles.layer.ground));
    let (_, current) = current_match(&roles);
    [[hit.r, hit.g, hit.b], [current.r, current.g, current.b]]
}

/// The current search hit's ink and fill.
#[must_use]
pub const fn current_match(roles: &Roles) -> (Rgb, Rgb) {
    (
        vt_rgb(roles.r#match.current_fg),
        vt_rgb(roles.r#match.current_bg),
    )
}

/// The canvas backdrop: `layer.ground`.
#[must_use]
pub fn ground(roles: &Roles) -> String {
    css(roles.layer.ground)
}

/// A pane border: `border.strong` for the focused pane, `border.muted` for
/// the rest, composited over the ground. Focus differs only in luminance.
#[must_use]
pub fn pane_border(roles: &Roles, active: bool) -> String {
    let flat = &roles.flat(Layer::Ground).border;
    css(if active { flat.strong } else { flat.muted })
}

/// The bell wash: `status.bell` at [`BELL_WASH_ALPHA`].
#[must_use]
pub fn bell_wash(roles: &Roles) -> String {
    css(with_alpha(roles.status.bell.rgb(), BELL_WASH_ALPHA))
}

/// An engine colour as canvas CSS.
#[must_use]
pub fn css_rgb(c: Rgb) -> String {
    format!("rgb({} {} {})", c.r, c.g, c.b)
}

/// A token colour as CSS: `rgb(r g b)`, with `/ a` when translucent.
#[must_use]
pub fn css(c: Rgba8) -> String {
    let mut out = format!("rgb({} {} {}", c.r, c.g, c.b);
    if !c.is_opaque() {
        let _ = write!(out, " / {:.3}", f64::from(c.a) / 255.0);
    }
    out.push(')');
    out
}

/// `var(--phux-<name>, <fallback>)`: the page's token when it loads
/// `phux-tokens.css`, else the house dark value, so an embedder that ships
/// no stylesheet still gets the house look.
#[must_use]
pub fn var(name: &str, fallback: impl std::fmt::Display) -> String {
    format!("var(--phux-{name}, {fallback})")
}

/// The house dark roles, which [`var`] falls back to.
#[must_use]
pub fn house() -> Roles {
    Roles::house(Mode::Dark)
}

/// A colour role as `var(--phux-<name>, <house dark value>)`.
#[must_use]
pub fn color(name: &str, fallback: Rgba8) -> String {
    var(name, css(fallback))
}

/// A length token as `var(--phux-<name>, <px>px)`.
#[must_use]
pub fn px(name: &str, fallback: f32) -> String {
    var(name, format!("{fallback}px"))
}

/// The `font` shorthand for a type role in the mono family, from the
/// page's tokens, else the generated values (`rem`, 1rem = 16px).
#[must_use]
pub fn mono_font(role: &str, r: &tokens::TypeRole) -> String {
    let rem = |pt: f32| format!("{}rem", pt / 16.0);
    let stack = tokens::TYPE
        .family
        .mono
        .stack
        .iter()
        .map(|face| {
            if face.contains(' ') {
                format!("\"{face}\"")
            } else {
                (*face).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{} {}/{} {}",
        var(&format!("type-{role}-weight"), r.weight),
        var(&format!("type-{role}-size"), rem(r.size)),
        var(&format!("type-{role}-line-height"), rem(r.line_height)),
        var("font-mono", stack),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Rgba8) -> Rgb {
        vt_rgb(c)
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn the_house_terminal_paints_the_authored_house_look() {
        for mode in [Mode::Dark, Mode::Light] {
            let t = house_terminal(mode);
            assert_eq!(*roles_for(rgb(t.bg), rgb(t.fg)), Roles::house(mode));
        }
        assert_eq!(house().layer.ground, tokens::TERMINAL.dark.bg);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn another_palette_is_derived_and_its_ground_is_the_terminal_background() {
        let (bg, fg) = (
            Rgb {
                r: 253,
                g: 246,
                b: 227,
            },
            Rgb {
                r: 101,
                g: 123,
                b: 131,
            },
        );
        let roles = roles_for(bg, fg);
        assert_eq!(roles.mode, Mode::Light, "a light ground solves light");
        assert_eq!(roles.layer.ground.rgb(), rgb8(bg));
        assert!(Rc::ptr_eq(&roles, &roles_for(bg, fg)), "solved once");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn an_engine_default_grid_adopts_the_house_palette() {
        let engine = (Rgb { r: 1, g: 2, b: 3 }, Rgb { r: 4, g: 5, b: 6 });
        let mut grid = Grid {
            cols: 0,
            rows: 0,
            default_fg: engine.1,
            default_bg: engine.0,
            cells: Vec::new(),
            cursor_col: 0,
            cursor_row: 0,
            cursor_visible: false,
        };
        adopt_house_palette(&mut grid, engine);
        assert_eq!(grid.default_bg, rgb(tokens::TERMINAL.dark.bg));
        let own = Rgb { r: 9, g: 9, b: 9 };
        grid.default_bg = own;
        adopt_house_palette(&mut grid, engine);
        assert_eq!(grid.default_bg, own, "a palette the program set is kept");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn more_contrast_paints_the_contrast_variant() {
        let t = house_terminal(Mode::Dark);
        set_more_contrast(true);
        let more = roles_for(rgb(t.bg), rgb(t.fg));
        set_more_contrast(false);
        assert_eq!(*more, Roles::house_contrast_more(Mode::Dark));
        assert_eq!(*roles_for(rgb(t.bg), rgb(t.fg)), Roles::house(Mode::Dark));
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn css_forms_carry_alpha_once() {
        assert_eq!(css(Rgba8::hex(0x1416_19FF)), "rgb(20 22 25)");
        assert_eq!(css(Rgba8::hex(0x0000_0080)), "rgb(0 0 0 / 0.502)");
        assert_eq!(var("layer-ground", "x"), "var(--phux-layer-ground, x)");
    }
}
