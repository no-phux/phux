use super::*;
use ratatui::style::Color;

/// The foreground of the dot (the first drawn cell) on the row naming `name`.
fn dot_fg(p: &SidebarPainter, name: &str) -> Color {
    let rect = r(0, 0, 24, 12);
    let buf = p.compose_buffer(rect, SidebarRule::Trailing, &[]);
    let y = (0..rect.h)
        .find(|y| row_text(&buf, rect, *y).contains(name))
        .expect("row");
    let x = (0..rect.w)
        .find(|x| !buf[(*x, y)].symbol().trim().is_empty())
        .expect("dot");
    buf[(x, y)].fg
}

#[test]
fn a_configured_session_colours_its_quiet_dot_and_only_that() {
    let theme = Theme::default();
    let sessions: Vec<phux_config::SessionConfigEntry> =
        toml::from_str::<phux_config::Config>("[[sessions]]\nname = 'api'\ncolor = 'coral'\n")
            .expect("parses")
            .sessions;
    let roster_rows = || vec![roster("api", 0, 1, 0), roster("web", 0, 1, 0)];
    let mut plain = SidebarPainter::new(theme);
    plain.set_roster(roster_rows());
    let mut p = SidebarPainter::new(theme).with_session_colors(sessions);
    p.set_roster(roster_rows());
    assert_eq!(dot_fg(&p, "api"), Color::Rgb(0xE8, 0x71, 0x5A));
    assert_eq!(dot_fg(&p, "web"), dot_fg(&plain, "web"));
    assert_eq!(dot_fg(&plain, "api"), theme.agent_idle);

    // A session that needs you keeps its state colour.
    p.set_roster(vec![roster("api", 1, 0, 0)]);
    assert_eq!(dot_fg(&p, "api"), theme.agent_blocked);
}
