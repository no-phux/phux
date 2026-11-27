//! Shared navigation identity: declared program glyph, declared tool mark, then
//! resource kind. Singles are flat; only actual multi-member tabs have edges.
use phux_design::State;
use phux_protocol::ids::ResourceKind;
use rune::{Color, El, Icon, el, text};

use super::sidebar::{face, state_of};
use super::{Paint, Tokens, icons};
use crate::activity::Mark;
use crate::addressable::Identity;

#[path = "identity_marks.rs"]
mod marks;

pub(in crate::ui) fn tells(mark: Mark) -> bool {
    mark == Mark::Bell
        || (mark != Mark::None
            && matches!(
                state_of(mark),
                State::NeedsYou | State::Failed | State::Done | State::Working
            ))
}

pub(in crate::ui) fn loudest(marks: impl IntoIterator<Item = Mark>) -> Option<Mark> {
    marks
        .into_iter()
        .filter(|m| tells(*m))
        .min_by_key(|m| state_of(*m).rank())
}

/// Only declared kind slugs select a logo. Names, titles and folders never do.
fn icon(identity: &Identity) -> &'static Icon {
    match identity.tool.as_deref() {
        Some("claude") => &marks::CLAUDE,
        Some("codex") => &marks::CODEX,
        Some("opencode") => &marks::OPENCODE,
        Some(_) => &icons::RESOURCE,
        None if identity.agent => &icons::RESOURCE,
        None => kind_icon(identity.kind),
    }
}

fn kind_icon(kind: ResourceKind) -> &'static Icon {
    match kind {
        ResourceKind::Terminal => &icons::PROMPT,
        ResourceKind::AccessibilityTree => &icons::TREE,
        ResourceKind::PixelSurface => &icons::WINDOW,
        ResourceKind::Document => &icons::DOCUMENT,
        _ => &icons::RESOURCE,
    }
}

fn glyph(identity: &Identity, t: &Tokens) -> El {
    let ink = t.roles.text.dim.paint();
    match &identity.glyph {
        Some(glyph) => text(glyph.clone())
            .key("identity-declared-glyph")
            .font(t.ty.label)
            .color(ink),
        None => rune::icon(icon(identity), t.icon.size.md)
            .key(icon(identity).name)
            .color(ink),
    }
}

/// `members` is actual tab membership, never session size or ancestry.
pub(in crate::ui) fn tile(
    identity: &Identity,
    mark: Mark,
    members: usize,
    ground: Color,
    t: &Tokens,
) -> El {
    let size = t.space.s5;
    let step = t.space.s0_5;
    let layers = members.saturating_sub(1).min(2);
    let mut base = el()
        .size(size + t.space.s1, size + t.space.s0_5)
        .a11y_hidden();
    for layer in (1..=layers).rev() {
        let offset = if layer == 1 { step } else { t.space.s1 };
        base = base.child(
            el().pin(Some(offset), Some(offset), None, None)
                .size(size, size)
                .radius(t.radius.xs)
                .fill(ground)
                .border(t.space.hairline, t.roles.border.muted.paint())
                .key(format!("identity-layer-{layer}"))
                .inert(),
        );
    }
    let front = el()
        .size(size, size)
        .radius(t.radius.xs)
        .center()
        .child(glyph(identity, t));
    let front = if layers > 0 {
        front
            .fill(ground)
            .border(t.space.hairline, t.roles.border.muted.paint())
    } else {
        front
    };
    base.child(front)
        .children(tells(mark).then(|| badge(mark, ground, t)))
}

fn badge(mark: Mark, ground: Color, t: &Tokens) -> El {
    let f = face(mark, t);
    // Large text grows the glyph, never past the tile it marks.
    let glyph = t.icon.size.xs.min(t.space.s3);
    el().pin(None, None, Some(-t.space.s0_5), Some(-t.space.s0_5))
        .size(glyph + t.space.s0_5, glyph + t.space.s0_5)
        .radius(t.radius.full)
        .fill(ground)
        .center()
        .inert()
        .child(rune::icon(f.icon, glyph).color(f.ink))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_state_worth_telling_gets_a_badge() {
        for mark in [
            Mark::Blocked,
            Mark::Failed,
            Mark::Done,
            Mark::Working,
            Mark::Bell,
        ] {
            assert!(tells(mark), "{mark:?}");
        }
        for mark in [Mark::None, Mark::Idle] {
            assert!(!tells(mark), "{mark:?}");
        }
        assert_eq!(
            loudest([Mark::Working, Mark::None, Mark::Blocked, Mark::Done]),
            Some(Mark::Blocked)
        );
        assert_eq!(loudest([Mark::None, Mark::Idle]), None);
    }

    #[test]
    fn declared_tools_and_resource_kinds_have_honest_fallbacks() {
        for (kind, name) in [
            (ResourceKind::Terminal, "phux-prompt"),
            (ResourceKind::Document, "phux-document"),
            (ResourceKind::AccessibilityTree, "phux-tree"),
            (ResourceKind::PixelSurface, "phux-window"),
            (ResourceKind::Unknown { tag: 90 }, "phux-resource"),
        ] {
            assert_eq!(icon(&Identity::kind(kind)).name, name);
        }
        for (tool, name) in [
            ("claude", "phux-identity-claude"),
            ("codex", "phux-identity-codex"),
            ("opencode", "phux-identity-opencode"),
            ("future-tool", "phux-resource"),
        ] {
            let mut identity = Identity::kind(ResourceKind::Terminal);
            identity.tool = Some(tool.to_owned());
            assert_eq!(icon(&identity).name, name);
        }
        let mut unnamed_kind = Identity::kind(ResourceKind::Terminal);
        unnamed_kind.agent = true;
        assert_eq!(icon(&unnamed_kind).name, "phux-resource");
    }

    fn identity_sheet(desk: &crate::desk::Desk, _: &mut rune::Cx<crate::desk::Desk>) -> El {
        let t = super::super::tokens_for(desk.sidebar_open, None);
        let ground = t.roles.layer.sidebar.paint();
        let mut rows = rune::col()
            .w_full()
            .h_full()
            .fill(ground)
            .px(t.space.s2)
            .py(t.space.s2);
        let identities = [
            ("Claude", ResourceKind::Terminal, Some("claude"), 3),
            ("Codex", ResourceKind::Terminal, Some("codex"), 2),
            ("OpenCode", ResourceKind::Terminal, Some("opencode"), 1),
            ("Terminal", ResourceKind::Terminal, None, 1),
            ("Tree mirror", ResourceKind::AccessibilityTree, None, 1),
            ("Surface mirror", ResourceKind::PixelSurface, None, 1),
            ("Document", ResourceKind::Document, None, 1),
            (
                "Unknown kind (identity only)",
                ResourceKind::Unknown { tag: 90 },
                None,
                1,
            ),
        ];
        for (name, kind, tool, members) in identities {
            let mut identity = Identity::kind(kind);
            identity.tool = tool.map(str::to_owned);
            rows = rows.child(
                rune::row()
                    .min_h(t.space.row_h)
                    .gap(t.space.icon_gap)
                    .items_center()
                    .child(tile(&identity, Mark::None, members, ground, &t))
                    .child(text(name).font(t.ty.label).color(t.roles.text.text.paint())),
            );
        }
        rows
    }

    /// Fixed bundled SVGs exercise Rune's actual rasterizer in both appearances.
    /// Unknown is deliberately only an identity fixture, never a screen target.
    #[test]
    fn native_resource_identity_render_frames() {
        let gpu = rune::render::Gpu::headless();
        for dark in [true, false] {
            let mut desk = crate::test_support::window();
            desk.sidebar_open = dark;
            let mut rt = crate::test_support::runtime(
                desk,
                identity_sheet,
                (320.0, 260.0),
                rune::Env::new(dark, 2.0),
            );
            crate::test_support::server::settle(&mut rt);
            assert!(rt.find("phux-identity-claude").is_some());
            assert!(rt.find("phux-identity-codex").is_some());
            assert!(rt.find("phux-identity-opencode").is_some());
            let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, (640, 520));
            let ground = pixels[..4].to_vec();
            assert!(
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|p| p.as_slice() != ground)
                    .count()
                    > 300
            );
            if let Some(dir) = std::env::var_os("RUNE_IDENTITY_SNAP") {
                let dir = std::path::Path::new(&dir);
                std::fs::create_dir_all(dir).unwrap();
                let mode = if dark { "dark" } else { "light" };
                rune::render::write_png(
                    &dir.join(format!("native-identities-{mode}.png")),
                    640,
                    520,
                    &pixels,
                )
                .unwrap();
            }
        }
    }
}
