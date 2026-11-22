//! The computed roles: status companions, match, diff and syntax from the
//! ANSI colours, and the flat forms. Shared by `derive` and the variants.

#![allow(clippy::suboptimal_flops)]

use super::{Core, Floors, on_fill, over, reach, slot, with_alpha};
use crate::color::{Rgb8, Rgba8};
use crate::roles::Roles;
use crate::tokens::{
    DiffRoles, FlatBorder, FlatDiff, FlatLayer, FlatMatch, FlatRoles, FlatScrim, FlatStatus,
    MatchRoles, Params, StatusRoles, WashRoles,
};

/// The computed roles from the core ones: status companions, match, diff
/// and syntax (from the ANSI colours) and the flat forms.
pub(super) fn complete(core: &Core, ansi: &[Rgb8; 16], p: &Params, floors: Floors) -> Roles {
    let (lut, n) = (p.srgb_lut, p.iterations);
    let extreme = core.mode.extreme();
    let raised = core.layer.raised.rgb();
    let sa = p.status_alpha;
    let c = |i: usize| -> [Rgba8; 4] {
        let t = core.tones[i];
        [
            t.opaque(),
            with_alpha(t, sa.wash),
            with_alpha(t, sa.edge),
            with_alpha(t, sa.glow),
        ]
    };
    let [needs_you, needs_you_wash, needs_you_edge, needs_you_glow] = c(0);
    let [working, working_wash, working_edge, working_glow] = c(1);
    let [done, done_wash, done_edge, done_glow] = c(2);
    let [failed, failed_wash, failed_edge, failed_glow] = c(3);
    let [idle, idle_wash, idle_edge, idle_glow] = c(4);
    let [unknown, unknown_wash, unknown_edge, unknown_glow] = c(5);
    let [bell, bell_wash, bell_edge, bell_glow] = c(6);
    let status = StatusRoles {
        needs_you,
        needs_you_wash,
        needs_you_edge,
        needs_you_glow,
        working,
        working_wash,
        working_edge,
        working_glow,
        done,
        done_wash,
        done_edge,
        done_glow,
        failed,
        failed_wash,
        failed_edge,
        failed_glow,
        idle,
        idle_wash,
        idle_edge,
        idle_glow,
        unknown,
        unknown_wash,
        unknown_edge,
        unknown_glow,
        bell,
        bell_wash,
        bell_edge,
        bell_glow,
    };
    let m = ansi[slot(p.slots.r#match)];
    let add = with_alpha(ansi[2], p.diff_alpha);
    let remove = with_alpha(ansi[1], p.diff_alpha);
    let base = floors.base_floor;
    let mut roles = Roles {
        mode: core.mode,
        layer: core.layer,
        text: core.text,
        border: core.border,
        wash: core.wash,
        accent: core.accent,
        status,
        selection: core.selection,
        r#match: MatchRoles {
            bg: with_alpha(m, p.match_alpha.bg),
            current_bg: m.opaque(),
            current_fg: on_fill(m, lut).opaque(),
        },
        diff: DiffRoles {
            add,
            remove,
            add_text: reach(ansi[2], extreme, base, &[over(add, raised)], lut, n).opaque(),
            remove_text: reach(ansi[1], extreme, base, &[over(remove, raised)], lut, n).opaque(),
        },
        syntax: ansi.map(|c| reach(c, extreme, base, &[raised], lut, n).opaque()),
        flat: empty_flat(),
    };
    roles.flat = flatten(&roles);
    roles
}

fn flat_layer(r: &Roles, base: Rgb8) -> FlatLayer {
    let o = |c: Rgba8| over(c, base).opaque();
    let s = &r.status;
    FlatLayer {
        layer: FlatScrim {
            scrim: o(r.layer.scrim),
        },
        border: FlatBorder {
            muted: o(r.border.muted),
            base: o(r.border.base),
            strong: o(r.border.strong),
        },
        wash: WashRoles {
            hover: o(r.wash.hover),
            press: o(r.wash.press),
            current: o(r.wash.current),
            selected: o(r.wash.selected),
        },
        status: FlatStatus {
            needs_you_wash: o(s.needs_you_wash),
            needs_you_edge: o(s.needs_you_edge),
            needs_you_glow: o(s.needs_you_glow),
            working_wash: o(s.working_wash),
            working_edge: o(s.working_edge),
            working_glow: o(s.working_glow),
            done_wash: o(s.done_wash),
            done_edge: o(s.done_edge),
            done_glow: o(s.done_glow),
            failed_wash: o(s.failed_wash),
            failed_edge: o(s.failed_edge),
            failed_glow: o(s.failed_glow),
            idle_wash: o(s.idle_wash),
            idle_edge: o(s.idle_edge),
            idle_glow: o(s.idle_glow),
            unknown_wash: o(s.unknown_wash),
            unknown_edge: o(s.unknown_edge),
            unknown_glow: o(s.unknown_glow),
            bell_wash: o(s.bell_wash),
            bell_edge: o(s.bell_edge),
            bell_glow: o(s.bell_glow),
        },
        r#match: FlatMatch {
            bg: o(r.r#match.bg),
        },
        diff: FlatDiff {
            add: o(r.diff.add),
            remove: o(r.diff.remove),
        },
    }
}

/// Every translucent role composited over each layer of T.
#[must_use]
pub fn flatten(r: &Roles) -> FlatRoles {
    FlatRoles {
        ground: flat_layer(r, r.layer.ground.rgb()),
        sidebar: flat_layer(r, r.layer.sidebar.rgb()),
        raised: flat_layer(r, r.layer.raised.rgb()),
        overlay: flat_layer(r, r.layer.overlay.rgb()),
    }
}

const fn empty_flat() -> FlatRoles {
    let blank = crate::tokens::COLOR.dark.flat.ground;
    FlatRoles {
        ground: blank,
        sidebar: blank,
        raised: blank,
        overlay: blank,
    }
}
