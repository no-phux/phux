use super::*;
use crate::connection::AgentAttention;
use crate::settings::theme::{palette, theme_by_id, themes};

fn agent(state: AgentState, attention: AgentAttention) -> AgentInfo {
    AgentInfo {
        name: "agent".into(),
        kind: None,
        state,
        attention,
        changed_at_ms: 0,
    }
}

#[test]
fn the_fleet_counts_each_state_and_who_needs_you() {
    let agents = [
        agent(AgentState::Working, AgentAttention::Normal),
        agent(AgentState::Working, AgentAttention::High),
        agent(AgentState::Blocked, AgentAttention::Normal),
        agent(AgentState::Done, AgentAttention::Low),
        agent(AgentState::Idle, AgentAttention::None),
        agent(AgentState::Unknown, AgentAttention::None),
    ];
    let fleet = fleet_of(&agents);
    assert_eq!(
        fleet,
        Fleet {
            working: 2,
            blocked: 1,
            done: 1,
            idle: 1,
            agents: 6,
            needs_you: 3,
        }
    );
    assert_eq!(fleet_of([]), Fleet::default());
}

#[test]
fn every_theme_reaches_the_shader_as_srgb() {
    for theme in themes() {
        let colors = palette(&theme_by_id(&theme.id, &[]));
        let uniforms = theme_uniforms(&colors, 0.5);
        assert_eq!(uniforms.intensity, 0.5);
        assert_eq!(
            uniforms.dark,
            if theme.appearance == "light" { 0. } else { 1. }
        );
        let accent = gpui::Rgba::from(from_hex(&theme.accent));
        assert_eq!(uniforms.accent, [accent.r, accent.g, accent.b, accent.a]);
        for color in std::iter::once(uniforms.background).chain(uniforms.ansi) {
            assert!(
                color.iter().all(|channel| (0. ..=1.).contains(channel)),
                "{}",
                theme.id
            );
        }
    }
}

#[test]
fn a_theme_without_a_palette_gets_the_default_ansi_colors() {
    let mut colors = palette(&theme_by_id(&themes()[0].id, &[]));
    colors.theme.palette = None;
    let uniforms = theme_uniforms(&colors, 2.);
    assert_eq!(uniforms.intensity, 1.);
    let red = gpui::Rgba::from(from_hex(DEFAULT_ANSI[1]));
    assert_eq!(uniforms.ansi[1], [red.r, red.g, red.b, red.a]);
}
