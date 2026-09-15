//! Attach intent (ADR-0127, `docs/spec/L1.md` §8.1): the one `role_policy`
//! byte an `ATTACH_RESOURCE` may end with and a session `ATTACH` may carry.
//!
//! A role is declared intent, not a second arbitration system: the input
//! lease (ADR-0033) stays the arbitration and the connection's scope grant
//! stays the security boundary. `VIEWER` makes the subscription
//! observe-only; `PRIMARY` with `DELIBERATE` takeover attaches and seizes the
//! lease in one step. Absent is `{ PRIMARY, NEVER }`, today's behaviour.

/// Which part a subscription plays on a Terminal (bit 0 of the byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TerminalRole {
    /// Input-capable, governed by the lease exactly as before roles existed.
    #[default]
    Primary = 0,
    /// Observe-only: the subscription's input is refused.
    Viewer = 1,
}

/// Whether the attach displaces the current lease holder (bit 1 of the byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TakeoverPolicy {
    /// Attach only; the lease is untouched.
    #[default]
    Never = 0,
    /// Attach and `ACQUIRE_INPUT { SEIZE }` in one step.
    Deliberate = 1,
}

/// The declared intent of one attach: a role and a takeover policy.
///
/// The wire form is one byte: bit 0 is the role, bit 1 the takeover; bits
/// 2-7 are reserved, sent as zero and ignored on receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct RolePolicy {
    /// The requested role.
    pub role: TerminalRole,
    /// The takeover policy.
    pub takeover: TakeoverPolicy,
}

impl RolePolicy {
    /// `{ PRIMARY, NEVER }`: what an absent byte means.
    pub const PRIMARY: Self = Self {
        role: TerminalRole::Primary,
        takeover: TakeoverPolicy::Never,
    };
    /// `{ VIEWER, NEVER }`: a watch-only subscription.
    pub const VIEWER: Self = Self {
        role: TerminalRole::Viewer,
        takeover: TakeoverPolicy::Never,
    };
    /// `{ PRIMARY, DELIBERATE }`: attach and take the wheel.
    pub const TAKEOVER: Self = Self {
        role: TerminalRole::Primary,
        takeover: TakeoverPolicy::Deliberate,
    };

    /// Bit 0: set for `VIEWER`.
    pub const VIEWER_BIT: u8 = 0x01;
    /// Bit 1: set for `DELIBERATE` takeover.
    pub const DELIBERATE_BIT: u8 = 0x02;

    /// The wire byte.
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        let role = match self.role {
            TerminalRole::Primary => 0,
            TerminalRole::Viewer => Self::VIEWER_BIT,
        };
        let takeover = match self.takeover {
            TakeoverPolicy::Never => 0,
            TakeoverPolicy::Deliberate => Self::DELIBERATE_BIT,
        };
        role | takeover
    }

    /// Read the wire byte. Reserved bits are ignored, so every byte decodes.
    #[must_use]
    pub const fn from_u8(byte: u8) -> Self {
        Self {
            role: if byte & Self::VIEWER_BIT == 0 {
                TerminalRole::Primary
            } else {
                TerminalRole::Viewer
            },
            takeover: if byte & Self::DELIBERATE_BIT == 0 {
                TakeoverPolicy::Never
            } else {
                TakeoverPolicy::Deliberate
            },
        }
    }

    /// Whether the subscription is observe-only.
    #[must_use]
    pub const fn is_viewer(self) -> bool {
        matches!(self.role, TerminalRole::Viewer)
    }

    /// Whether the attach seizes the input lease.
    #[must_use]
    pub const fn takes_over(self) -> bool {
        matches!(self.takeover, TakeoverPolicy::Deliberate)
    }

    /// `false` for `{ VIEWER, DELIBERATE }`: a viewer cannot take the wheel,
    /// and a receiver refuses the attach rather than guess which half was
    /// meant.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        !(self.is_viewer() && self.takes_over())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_byte_round_trips_every_defined_policy() {
        for policy in [
            RolePolicy::PRIMARY,
            RolePolicy::VIEWER,
            RolePolicy::TAKEOVER,
            RolePolicy::from_u8(0x03),
        ] {
            assert_eq!(RolePolicy::from_u8(policy.to_u8()), policy);
        }
        assert_eq!(RolePolicy::PRIMARY.to_u8(), 0);
        assert_eq!(RolePolicy::VIEWER.to_u8(), 1);
        assert_eq!(RolePolicy::TAKEOVER.to_u8(), 2);
        assert_eq!(RolePolicy::default(), RolePolicy::PRIMARY);
    }

    #[test]
    fn reserved_bits_are_ignored_and_a_deliberate_viewer_is_invalid() {
        assert_eq!(RolePolicy::from_u8(0xfc), RolePolicy::PRIMARY);
        assert_eq!(RolePolicy::from_u8(0xfd), RolePolicy::VIEWER);
        assert!(!RolePolicy::from_u8(0x03).is_valid());
        assert!(RolePolicy::TAKEOVER.is_valid() && RolePolicy::VIEWER.is_valid());
    }
}
