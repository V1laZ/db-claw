//! DB Claw — a fast native database manager for PostgreSQL and MongoDB.

mod actions;
mod connection_dialog;
mod control;
mod console;
mod grid;
mod navigator;
mod record_dialog;
mod state;
mod store;
mod table_view;
mod util;
mod workspace;

use gpui_kit::component::Theme;
use gpui_kit::*;

use actions::*;

/// `secondary-` = Cmd on macOS, Ctrl elsewhere (gpui only resolves it when
/// the OS is known, which `KeyBinding::new` doesn't pass).
fn platform_keys(keys: &'static str) -> &'static str {
    let modifier = if cfg!(target_os = "macos") { "cmd-" } else { "ctrl-" };
    Box::leak(keys.replace("secondary-", modifier).into_boxed_str())
}

fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new(platform_keys("secondary-q"), Quit, None),
        KeyBinding::new(platform_keys("secondary-n"), NewConnection, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-t"), NewConsole, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-w"), CloseTab, Some("Workspace")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Workspace")),
        KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-shift-]"), NextTab, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-shift-["), PrevTab, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-b"), ToggleSidebar, Some("Workspace")),
        KeyBinding::new("alt-left", GoBack, Some("Workspace && !Input")),
        KeyBinding::new("alt-right", GoForward, Some("Workspace && !Input")),
        KeyBinding::new(platform_keys("secondary-p"), SearchTables, Some("Workspace")),
        KeyBinding::new(platform_keys("secondary-s"), SaveChanges, Some("TableView")),
        KeyBinding::new(platform_keys("secondary-r"), Refresh, Some("TableView")),
        KeyBinding::new("f5", Refresh, Some("TableView")),
        KeyBinding::new(platform_keys("secondary-f"), FocusFilter, Some("TableView")),
        KeyBinding::new(platform_keys("secondary-backspace"), DeleteRow, Some("TableView && !Input")),
        KeyBinding::new(platform_keys("secondary-i"), InsertRow, Some("TableView")),
        KeyBinding::new("enter", EditCell, Some("TableView && !Input")),
        KeyBinding::new("f2", EditCell, Some("TableView && !Input")),
        KeyBinding::new(platform_keys("secondary-shift-n"), SetNull, Some("TableView && !Input")),
        KeyBinding::new(platform_keys("secondary-c"), CopyValue, Some("TableView && !Input")),
        KeyBinding::new("escape", CancelEdit, Some("TableView")),
        KeyBinding::new(platform_keys("secondary-]"), NextPage, Some("TableView && !Input")),
        KeyBinding::new(platform_keys("secondary-["), PrevPage, Some("TableView && !Input")),
        KeyBinding::new(platform_keys("secondary-enter"), RunQuery, Some("Console")),
        KeyBinding::new(platform_keys("secondary-shift-enter"), RunAll, Some("Console")),
        KeyBinding::new(platform_keys("secondary-."), CancelQuery, Some("Console")),
    ]
}

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            Theme::sync_system_appearance(None, cx);
            cx.bind_keys(key_bindings());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus(vec![
                Menu {
                    name: "DB Claw".into(),
                    disabled: false,
                    items: vec![MenuItem::action("Quit DB Claw", Quit)],
                },
                Menu {
                    name: "File".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("New Connection…", NewConnection),
                        MenuItem::action("New Console", NewConsole),
                        MenuItem::separator(),
                        MenuItem::action("Close Tab", CloseTab),
                    ],
                },
                Menu {
                    name: "View".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Toggle Sidebar", ToggleSidebar),
                        MenuItem::action("Search Tables", SearchTables),
                    ],
                },
            ]);
            let bounds = Bounds::centered(None, size(px(1400.), px(880.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions { title: Some("DB Claw".into()), ..Default::default() }),
                window_min_size: Some(size(px(720.), px(480.))),
                ..Default::default()
            };
            let (window, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
                window
                    .observe_window_appearance(|window, cx| {
                        Theme::sync_system_appearance(Some(window), cx);
                    })
                    .detach();
                cx.new(|cx| workspace::Workspace::new(window, cx))
            })
            .expect("failed to open the main window");
            if control::enabled() {
                control::start(workspace, window, cx);
            }
            cx.activate(true);
        });
}
