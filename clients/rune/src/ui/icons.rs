//! The window's icons: one stroke on a 16-point grid, tinted with `.color()`.

use rune::Icon;

macro_rules! icon {
    ($id:ident, $name:literal, $path:literal) => {
        pub static $id: Icon = Icon {
            name: $name,
            view: 16.0,
            body: concat!(
                r#"<path d=""#,
                $path,
                r#"" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/>"#
            ),
        };
    };
}

icon!(CLOSE, "phux-close", "M4.5 4.5l7 7M11.5 4.5l-7 7");
icon!(PLUS, "phux-plus", "M8 3.5v9M3.5 8h9");
icon!(CHEVRON, "phux-chevron", "M4.5 6.5L8 10l3.5-3.5");
icon!(
    CHEVRON_RIGHT,
    "phux-chevron-right",
    "M6.5 4.5L10 8l-3.5 3.5"
);
icon!(
    SEARCH,
    "phux-search",
    "M7 11.5a4.5 4.5 0 1 0 0-9 4.5 4.5 0 0 0 0 9zM10.5 10.5l3 3"
);
icon!(
    SIDEBAR,
    "phux-sidebar",
    "M3.5 2.5h9a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-9a1 1 0 0 1 1-1zM6.5 2.5v11"
);
icon!(
    INBOX,
    "phux-inbox",
    "M2.5 9.5h3l1 2h3l1-2h3M4.5 3.5h7l2 6v3a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-3z"
);
icon!(CHEVRON_LEFT, "phux-chevron-left", "M9.5 3.5L5 8l4.5 4.5");
icon!(
    MORE,
    "phux-more",
    "M4.5 8a1 1 0 1 1-2 0 1 1 0 0 1 2 0zM9 8a1 1 0 1 1-2 0 1 1 0 0 1 2 0zM13.5 8a1 1 0 1 1-2 0 1 1 0 0 1 2 0z"
);
icon!(PROMPT, "phux-prompt", "M3 5l3 3-3 3M8 11.5h5");
icon!(
    KEYBOARD,
    "phux-keyboard",
    "M2.5 4.5h11a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1zM4.5 7h.01M7 7h.01M9 7h.01M11.5 7h.01M5.5 9.5h5M6 14l2 1.5 2-1.5"
);
icon!(
    PAD,
    "phux-pad",
    "M8 1.5l1.8 2H6.2zM8 14.5l1.8-2H6.2zM1.5 8l2-1.8v3.6zM14.5 8l-2-1.8v3.6zM8 10a2 2 0 1 0 0-4 2 2 0 0 0 0 4z"
);
icon!(SHIFT, "phux-shift", "M8 2.5l5.5 6H10.5v5h-5v-5H2.5z");
icon!(
    BACKSPACE,
    "phux-backspace",
    "M5.5 3.5h8a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-8L1.5 8zM7.5 6l4 4M11.5 6l-4 4"
);
icon!(
    RETURN,
    "phux-return",
    "M13 3.5v5a1 1 0 0 1-1 1H3.5M6 6.5l-3 3 3 3"
);
