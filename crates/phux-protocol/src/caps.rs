//! Capability advertisements (SPEC §6.2).
//!
//! Capabilities live in HELLO and apply for the life of the connection. The
//! server rewrites outbound SGR bytes to the advertised [`ColorSupport`]
//! (ADR-0013).

/// A `u8` wire bit-set of `$item` flags whose `Default` is every known flag.
/// Unknown bits are dropped on decode.
macro_rules! u8_flag_set {
    ($(#[$doc:meta])* $name:ident of $item:ident { $($variant:ident),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(u8);

        impl $name {
            const KNOWN: u8 = 0 $(| ($item::$variant as u8))+;

            /// The empty set.
            #[must_use]
            pub const fn new() -> Self {
                Self(0)
            }

            /// Every known flag.
            #[must_use]
            pub const fn all() -> Self {
                Self(Self::KNOWN)
            }

            /// The set of `items`.
            #[must_use]
            pub const fn with(items: &[$item]) -> Self {
                let mut bits = 0;
                let mut i = 0;
                while i < items.len() {
                    bits |= items[i] as u8;
                    i += 1;
                }
                Self(bits)
            }

            /// Whether `item` is in the set.
            #[must_use]
            pub const fn contains(self, item: $item) -> bool {
                self.0 & (item as u8) != 0
            }

            /// Known wire bits.
            #[must_use]
            pub const fn as_wire(self) -> u8 {
                self.0 & Self::KNOWN
            }

            /// Decode known bits, ignoring future ones.
            #[must_use]
            pub const fn from_wire(bits: u8) -> Self {
                Self(bits & Self::KNOWN)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::all()
            }
        }
    };
}

mod bootstrap;
mod features;
mod hello;
mod limits;
mod output;

pub use bootstrap::*;
pub use features::*;
pub use hello::*;
pub use limits::*;
pub use output::*;
