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
