//! Colour tags for connections (e.g. red = production), shown in the sidebar and on
//! tabs. Stored as preset names so they follow the theme (light and dark).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

/// (stored name, label)
pub const PRESETS: &[(&str, &str)] = &[
    ("red", "Red"),
    ("orange", "Orange"),
    ("yellow", "Yellow"),
    ("green", "Green"),
    ("cyan", "Cyan"),
    ("blue", "Blue"),
    ("purple", "Purple"),
];

/// The colour for a preset name; unknown names (e.g. from a newer version) show none.
pub fn resolve(name: Option<&str>, cx: &App) -> Option<Hsla> {
    let theme = cx.theme();
    Some(match name? {
        "red" => theme.red,
        "orange" => hsla(25. / 360., 0.92, 0.55, 1.),
        "yellow" => theme.yellow,
        "green" => theme.green,
        "cyan" => theme.cyan,
        "blue" => theme.blue,
        "purple" => theme.magenta,
        _ => return None,
    })
}

/// The colour of connection `conn_id`, if it has one.
pub fn of_connection(state: &Entity<crate::state::AppState>, conn_id: &str, cx: &App) -> Option<Hsla> {
    let name = state.read(cx).config(conn_id)?.color.clone();
    resolve(name.as_deref(), cx)
}
