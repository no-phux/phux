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

icon!(
    SETTINGS,
    "phux-settings",
    "M2 4h12M2 8h12M2 12h12M5 2.5v3M11 6.5v3M6 10.5v3"
);
icon!(CLOSE, "phux-close", "M4.5 4.5l7 7M11.5 4.5l-7 7");
icon!(PLUS, "phux-plus", "M8 4v8M4 8h8");
icon!(
    LOCK,
    "phux-lock",
    "M4.5 7.5h7v6h-7zM5.5 7.5V5.5a2.5 2.5 0 0 1 5 0v2"
);
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
icon!(CHECK, "phux-check", "M3.5 8.5l3 3 6-7");
icon!(ARROW_RIGHT, "phux-arrow-right", "M3 8h10M9 4l4 4-4 4");
icon!(ARROW_UP, "phux-arrow-up", "M8 13V3M4 7l4-4 4 4");
icon!(ARROW_DOWN, "phux-arrow-down", "M8 3v10M4 9l4 4 4-4");
icon!(BOLT, "phux-bolt", "M9 2L3.5 9h4L7 14l5.5-7h-4z");
icon!(
    WINDOW,
    "phux-window",
    "M3.5 3h9a1 1 0 0 1 1 1v8a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1zM2.5 6h11"
);
icon!(
    FOLDER,
    "phux-folder",
    "M2.5 4.5a1 1 0 0 1 1-1h2.5L7.5 5h5a1 1 0 0 1 1 1v5.5a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"
);
icon!(ARROW_LEFT, "phux-arrow-left", "M13 8H3M7 4L3 8l4 4");
icon!(
    PUZZLE,
    "phux-puzzle",
    "M3 5.5h2.5a1.5 1.5 0 1 1 3 0H11v2.5a1.5 1.5 0 1 1 0 3V13H3z"
);
icon!(
    HAND,
    "phux-hand",
    "M5.5 8V3.5a1 1 0 0 1 2 0V7.5M7.5 7V2.5a1 1 0 0 1 2 0V7.5M9.5 7.5V3.5a1 1 0 0 1 2 0V9.5c0 2.5-1.5 4.5-4 4.5S4 12.5 3 10.5L2 8.5a1 1 0 0 1 1.7-1L5.5 9.5"
);
icon!(
    BRING,
    "phux-bring",
    "M1.5 8h1M4 8h1M6.5 8h7M10.5 4.5L14 8l-3.5 3.5"
);
icon!(
    SKIP,
    "phux-skip",
    "M8 14a6 6 0 1 0 0-12 6 6 0 0 0 0 12zM3.8 3.8l8.4 8.4"
);
icon!(
    PALETTE,
    "phux-palette",
    "M8 14a6 6 0 1 1 6-6c0 1.5-1 2.5-2.5 2.5H10a1.2 1.2 0 0 0-.8 2.1c.4.4.3 1.4-1.2 1.4zM5 7h.01M7 4.8h.01M10 5h.01"
);

icon!(
    BELL,
    "phux-bell",
    "M4 11.5V7a4 4 0 0 1 8 0v4.5l1 1H3zM6.5 14h3"
);
