//! `From` impls for the Wire mirrors that only copy fields and variants.
//!
//! Only the impl blocks are generated. Every `Wire*` type stays a plain
//! `#[derive(uniffi::Enum/Record)]` definition, so the foreign surface
//! (names, field order, docs, defaults) never depends on these macros.
//!
//! Enum matches list every source variant and have no wildcard arm, and
//! record impls destructure the whole source, so a new protocol variant or
//! field is a compile error here rather than a silent drop.

/// Converts one source field into its Wire form.
///
/// No mode copies the value; `into` converts it; `opt` converts an
/// `Option`'s payload; `vec` converts every element of a collection.
macro_rules! mirror_convert {
    ($value:expr) => {
        $value
    };
    ($value:expr, into) => {
        $value.into()
    };
    ($value:expr, opt) => {
        $value.map(::core::convert::Into::into)
    };
    ($value:expr, vec) => {
        $value
            .into_iter()
            .map(::core::convert::Into::into)
            .collect()
    };
}

/// `From<Src> for Wire` for a record: `f` copies, `f: into|opt|vec`
/// converts (see `mirror_convert!`), `src as dst` renames.
///
/// ```ignore
/// mirror_from!(PathAnswer => WirePathAnswer {
///     request_id, rows: vec, failure as error: opt, message,
/// });
/// ```
macro_rules! mirror_from {
    ($src:ty => $dst:ty { $($fields:tt)* }) => {
        mirror_from!(@fields [$src => $dst] [] $($fields)*);
    };
    // `src as dst`, optionally converted.
    (@fields $head:tt [$($done:tt)*]
        $from:ident as $to:ident $(: $mode:ident)? $(, $($rest:tt)*)?) => {
        mirror_from!(@fields $head [$($done)* [$from $to $($mode)?]] $($($rest)*)?);
    };
    // `f`, optionally converted.
    (@fields $head:tt [$($done:tt)*] $field:ident $(: $mode:ident)? $(, $($rest:tt)*)?) => {
        mirror_from!(@fields $head [$($done)* [$field $field $($mode)?]] $($($rest)*)?);
    };
    (@fields [$src:ty => $dst:ty] [$([$from:ident $to:ident $($mode:ident)?])*]) => {
        impl From<$src> for $dst {
            fn from(value: $src) -> Self {
                type Source = $src;
                let Source { $($from),* } = value;
                Self {
                    $($to: mirror_convert!($from $(, $mode)?),)*
                }
            }
        }
    };
}

/// `From<Src> for Wire` for an enum. Arms: `A` (same name), `A => B`
/// (rename), `A(x) => B { y }` (tuple payload to named fields), and
/// `A { f, g: into }` (same-named struct variant, fields as in
/// `mirror_convert!`). `Src <=> Wire` takes unit and rename arms only and
/// also emits the reverse impl.
///
/// ```ignore
/// mirror_enum_from!(SplitDir <=> WireSplitDir {
///     Horizontal => SideBySide,
///     Vertical => Stacked,
/// });
/// ```
macro_rules! mirror_enum_from {
    ($src:ident <=> $dst:ident { $($arms:tt)* }) => {
        mirror_enum_from!(@units [$src $dst] [] [] $($arms)*);
    };
    ($src:ty => $dst:ty { $($arms:tt)* }) => {
        mirror_enum_from!(@arms [$src => $dst] [] $($arms)*);
    };

    // Unit arms of `<=>`: forward and reverse lists side by side.
    (@units $head:tt [$($fwd:tt)*] [$($back:tt)*]
        $from:ident => $to:ident $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@units $head
            [$($fwd)* [($from) ($to)]] [$($back)* [($to) ($from)]] $($($rest)*)?);
    };
    (@units $head:tt [$($fwd:tt)*] [$($back:tt)*] $same:ident $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@units $head
            [$($fwd)* [($same) ($same)]] [$($back)* [($same) ($same)]] $($($rest)*)?);
    };
    (@units [$src:ident $dst:ident] [$($fwd:tt)*] [$($back:tt)*]) => {
        mirror_enum_from!(@emit [$src => $dst] [$($fwd)*]);
        mirror_enum_from!(@emit [$dst => $src] [$($back)*]);
    };

    // One-way arms, each normalized to `[(source pattern) (Self expression)]`.
    (@arms $head:tt [$($done:tt)*]
        $from:ident ($($bind:ident),*) => $to:ident { $($field:ident),* }
        $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@arms $head
            [$($done)* [($from($($bind),*)) ($to { $($field: $bind),* })]] $($($rest)*)?);
    };
    (@arms $head:tt [$($done:tt)*]
        $variant:ident { $($field:ident $(: $mode:ident)?),* $(,)? }
        $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@arms $head [$($done)* [
            ($variant { $($field),* })
            ($variant { $($field: mirror_convert!($field $(, $mode)?)),* })
        ]] $($($rest)*)?);
    };
    (@arms $head:tt [$($done:tt)*] $from:ident => $to:ident $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@arms $head [$($done)* [($from) ($to)]] $($($rest)*)?);
    };
    (@arms $head:tt [$($done:tt)*] $same:ident $(, $($rest:tt)*)?) => {
        mirror_enum_from!(@arms $head [$($done)* [($same) ($same)]] $($($rest)*)?);
    };
    (@arms $head:tt [$($done:tt)*]) => {
        mirror_enum_from!(@emit $head [$($done)*]);
    };

    (@emit [$src:ty => $dst:ty] [$([($($pat:tt)*) ($($expr:tt)*)])*]) => {
        impl From<$src> for $dst {
            fn from(value: $src) -> Self {
                type Source = $src;
                match value {
                    $(Source::$($pat)* => Self::$($expr)*,)*
                }
            }
        }
    };
}
