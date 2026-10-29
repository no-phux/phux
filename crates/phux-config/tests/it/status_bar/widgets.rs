//! Integration tests for `phux_config::widget`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phux_config::WidgetSpec;
use phux_config::widget::{
    CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetRegistry,
    WindowBadge, WindowInfo,
};

mod kinds;
mod opts;
mod width;

fn opts_with(entries: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn fixed_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(12345)
}

fn build_spec(
    kind: &str,
    opts: &[(&str, toml::Value)],
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let spec = WidgetSpec {
        kind: kind.to_owned(),
        opts: opts_with(opts),
    };
    WidgetRegistry::with_builtins().build(&spec)
}

fn text_of(cells: &WidgetCells) -> String {
    cells.cells.iter().filter_map(|c| c.text.first()).collect()
}

fn style_table(entries: &[(&str, toml::Value)]) -> toml::Value {
    let mut t = toml::value::Table::new();
    for (k, v) in entries {
        t.insert((*k).to_owned(), v.clone());
    }
    toml::Value::Table(t)
}

fn win(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        ..WindowInfo::default()
    }
}

fn render_windows(opts: &[(&str, toml::Value)], windows: &[WindowInfo]) -> WidgetCells {
    let w = build_spec("windows", opts).expect("windows builds");
    w.render(&WidgetContext::new(fixed_time(), "", "C-a", windows))
}
