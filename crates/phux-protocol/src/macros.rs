//! Declarative macros shared by the wire types.

/// Declare a fieldless `#[repr(u8)]` wire enum with its `$to` / `$from` byte
/// conversions and `ALL`, every value in declaration order. `$from` is
/// `None` for a byte this build does not define, or with `else $fallback`
/// total, reading such a byte as `$fallback`.
macro_rules! wire_enum {
    ($to:ident / $from:ident $(else $fallback:ident)?;
     $(#[$meta:meta])*
     pub enum $name:ident {
         $( $(#[$vmeta:meta])* $variant:ident = $tag:literal, )*
     }) => {
        $(#[$meta])*
        #[repr(u8)]
        pub enum $name {
            $( $(#[$vmeta])* $variant = $tag, )*
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: [Self; [$(stringify!($variant)),*].len()] = [$(Self::$variant),*];

            /// Wire byte for this value.
            #[must_use]
            pub const fn $to(self) -> u8 {
                self as u8
            }

            wire_enum!(@from $from $($fallback)?; $($tag => $variant),*);
        }
    };
    (@from $from:ident; $($tag:literal => $variant:ident),*) => {
        /// Decode a wire byte; `None` for a value this build does not define.
        #[must_use]
        pub const fn $from(value: u8) -> Option<Self> {
            match value {
                $( $tag => Some(Self::$variant), )*
                _ => None,
            }
        }
    };
    (@from $from:ident $fallback:ident; $($tag:literal => $variant:ident),*) => {
        /// Decode a wire byte; a value this build does not define reads as
        #[doc = concat!("[`Self::", stringify!($fallback), "`].")]
        #[must_use]
        pub const fn $from(value: u8) -> Self {
            match value {
                $( $tag => Self::$variant, )*
                _ => Self::$fallback,
            }
        }
    };
}

/// [`wire_enum!`] for an open enum: a value this build does not define
/// decodes as `Unknown(raw)`, listed first, which encodes back as `raw`, so
/// the conversion is total both ways.
macro_rules! open_wire_enum {
    ($repr:ty; $to:ident / $from:ident;
     $(#[$meta:meta])*
     pub enum $name:ident {
         $(#[$umeta:meta])* Unknown($unknown:ty),
         $( $(#[$vmeta:meta])* $variant:ident = $tag:literal, )*
     }) => {
        $(#[$meta])*
        pub enum $name {
            $( $(#[$vmeta])* $variant, )*
            $(#[$umeta])* Unknown($unknown),
        }

        impl $name {
            /// The wire value; `Unknown` writes back its raw value.
            #[must_use]
            pub const fn $to(self) -> $repr {
                match self {
                    $( Self::$variant => $tag, )*
                    Self::Unknown(value) => value,
                }
            }

            /// Classify a wire value, keeping one this build does not define.
            #[must_use]
            pub const fn $from(value: $repr) -> Self {
                match value {
                    $( $tag => Self::$variant, )*
                    other => Self::Unknown(other),
                }
            }

            /// The value this build defines for `value`, or `None`: what a
            /// request decoder refuses as `UNSUPPORTED`.
            #[must_use]
            pub const fn known(value: $repr) -> Option<Self> {
                match value {
                    $( $tag => Some(Self::$variant), )*
                    _ => None,
                }
            }
        }
    };
}

/// Match each body-level TLV field of `$dec`, in wire order, against the
/// arms given, with `$value` bound to the field's bytes. An id no arm names
/// is skipped by its length (`docs/protocol.md#framing`).
macro_rules! for_each_field {
    ($dec:expr, $value:ident, { $($arms:tt)* }) => {
        while let Some((id, $value)) = $dec.read_field()? {
            match id {
                $($arms)*
                _ => {}
            }
        }
    };
}

/// Declare `#[must_use]` builder setters that move one argument into one
/// field and return `self`. Each entry reads
/// `[/// doc] [const] fn with_x(param: T) => field;` and expands to
/// `pub [const] fn with_x(mut self, param: T) -> Self { self.field = param; self }`.
/// An entry without a doc line is documented as
/// "Builder setter for [`Self::field`]." The setters are generated, so a grep
/// for `fn with_x` finds them here, not at their definition: grep the entry
/// (`with_x(`) instead.
macro_rules! builder_setters {
    (@setter [] [$($c:tt)?] $name:ident($param:ident: $t:ty) => $field:ident) => {
        builder_setters!(@setter
            [concat!("Builder setter for [`Self::", stringify!($field), "`].")]
            [$($c)?] $name($param: $t) => $field);
    };
    (@setter [$doc:expr] [$($c:tt)?] $name:ident($param:ident: $t:ty) => $field:ident) => {
        #[doc = $doc]
        #[must_use]
        pub $($c)? fn $name(mut self, $param: $t) -> Self {
            self.$field = $param;
            self
        }
    };
    ($(#[doc = $doc:literal])? const fn $name:ident($param:ident: $t:ty) => $field:ident;
     $($rest:tt)*) => {
        builder_setters!(@setter [$($doc)?] [const] $name($param: $t) => $field);
        builder_setters!($($rest)*);
    };
    ($(#[doc = $doc:literal])? fn $name:ident($param:ident: $t:ty) => $field:ident;
     $($rest:tt)*) => {
        builder_setters!(@setter [$($doc)?] [] $name($param: $t) => $field);
        builder_setters!($($rest)*);
    };
    () => {};
}
