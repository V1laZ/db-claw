//! Main window layout: navigator sidebar + tabbed data / console views.

use dbclaw_core::model::{DbKind, ObjectKind, ObjectRef};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    h_resizable,
    resizable_panel, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::*;
use crate::connection_dialog;
use crate::console::ConsoleView;
use crate::navigator::{NavEvent, Navigator};
use crate::state::{AppEvent, AppState};
use crate::table_view::{TableSource, TableView, TableViewEvent};

/// Same table, ignoring the object kind (FK targets don't know if a table
/// is partitioned the way the navigator does).
fn same_table(a: &ObjectRef, b: &ObjectRef) -> bool {
    a.database == b.database && a.schema == b.schema && a.name == b.name
}

#[derive(Clone)]
pub enum Tab {
    Table(Entity<TableView>),
    Console(Entity<ConsoleView>),
}

impl Tab {
    fn conn_id(&self, cx: &App) -> String {
        match self {
            Tab::Table(t) => t.read(cx).conn_id.clone(),
            Tab::Console(c) => c.read(cx).conn_id.clone(),
        }
    }

    fn entity_id(&self) -> EntityId {
        match self {
            Tab::Table(t) => t.entity_id(),
            Tab::Console(c) => c.entity_id(),
        }
    }
}

pub struct Workspace {
    state: Entity<AppState>,
    navigator: Entity<Navigator>,
    tabs: Vec<Tab>,
    active: usize,
    sidebar: bool,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|_| AppState::new());
        let navigator = cx.new(|cx| Navigator::new(state.clone(), window, cx));
        let subs = vec![
            cx.subscribe_in(&navigator, window, Self::on_nav_event),
            cx.subscribe_in(&state, window, |this, _, _: &AppEvent, _, cx| {
                // Close tabs of connections that were removed or disconnected.
                {
                    let state = this.state.read(cx);
                    let live: Vec<String> = this
                        .tabs
                        .iter()
                        .map(|t| t.conn_id(cx))
                        .filter(|id| state.connection(id).is_none())
                        .collect();
                    if !live.is_empty() {
                        this.tabs.retain(|t| !live.contains(&t.conn_id(cx)));
                        this.active = this.active.min(this.tabs.len().saturating_sub(1));
                    }
                    cx.notify();
                }
            }),
        ];
        // Focus the workspace so app shortcuts (⌘N, ⌘T, …) work right away.
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            state,
            navigator,
            tabs: Vec::new(),
            active: 0,
            sidebar: true,
            focus,
            _subs: subs,
        }
    }

    fn on_nav_event(&mut self, _: &Entity<Navigator>, ev: &NavEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            NavEvent::OpenObject { conn_id, object } => self.open_object(conn_id, object, None, window, cx),
            NavEvent::OpenConsole { conn_id, database, collection } => {
                self.open_console(conn_id, database.clone(), collection.clone(), window, cx)
            }
            NavEvent::NewConnection => connection_dialog::open(None, self.state.clone(), window, cx),
            NavEvent::EditConnection(id) => {
                let config = self.state.read(cx).config(id).cloned();
                if let Some(config) = config {
                    connection_dialog::open(Some(config), self.state.clone(), window, cx);
                }
            }
            NavEvent::DeleteConnection(id) => self.confirm_delete(id.clone(), window, cx),
        }
    }

    fn confirm_delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.state.read(cx).config(&id).map(|c| c.name.clone()) else { return };
        let state = self.state.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let state = state.clone();
            let id = id.clone();
            dialog
                .title("Delete connection")
                .child(format!("Remove “{name}” from saved connections? The database itself is not touched."))
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(Button::new("cancel").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                        .child(Button::new("delete").danger().label("Delete").on_click(move |_, w, cx| {
                            state.update(cx, |s, cx| s.remove(&id, cx));
                            w.close_dialog(cx);
                        })),
                )
        });
    }

    /// Open (or switch to) the tab of a table. With `filter`, the tab shows
    /// only matching rows (used when following foreign keys).
    pub fn open_object(
        &mut self,
        conn_id: &str,
        object: &ObjectRef,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.tabs.iter().position(|t| match t {
            Tab::Table(t) => {
                let t = t.read(cx);
                t.conn_id == conn_id && same_table(&t.object, object)
            }
            _ => false,
        }) {
            if let (Some(filter), Tab::Table(t)) = (filter, &self.tabs[ix]) {
                t.update(cx, |t, cx| t.set_filter_text(&filter, window, cx));
            }
            self.activate(ix, window, cx);
            return;
        }
        let state = self.state.read(cx);
        let Some(conn) = state.connection(conn_id) else { return };
        let Some(config) = state.config(conn_id) else { return };
        let (name, read_only) = (SharedString::from(config.name.clone()), config.read_only);
        let (conn_id, object) = (conn_id.to_string(), object.clone());
        let source = TableSource { conn_id, conn_name: name, conn, read_only };
        let view = cx.new(|cx| TableView::new(source, object, filter, window, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, ev: &TableViewEvent, window, cx| match ev {
            TableViewEvent::OpenReference { conn_id, object, filter } => {
                this.open_object(conn_id, object, Some(filter.clone()), window, cx)
            }
        });
        self._subs.push(sub);
        let ix = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(ix, Tab::Table(view));
        self.activate(ix, window, cx);
    }

    pub fn open_console(
        &mut self,
        conn_id: &str,
        database: String,
        collection: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.read(cx);
        let Some(conn) = state.connection(conn_id) else {
            window.push_notification(Notification::warning("Connect first"), cx);
            return;
        };
        let name = SharedString::from(state.config(conn_id).map(|c| c.name.clone()).unwrap_or_default());
        let database = if database.is_empty() {
            conn.default_database().unwrap_or_else(|| "admin".into())
        } else {
            database
        };
        let conn_id = conn_id.to_string();
        let view = cx.new(|cx| ConsoleView::new(conn_id, name, conn, database, collection, window, cx));
        self.tabs.push(Tab::Console(view.clone()));
        let ix = self.tabs.len() - 1;
        self.activate(ix, window, cx);
        let h = view.read(cx).editor_focus(cx);
        h.focus(window, cx);
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = ix;
        match &self.tabs[ix] {
            Tab::Table(t) => {
                let (conn_id, object) = {
                    let t = t.read(cx);
                    (t.conn_id.clone(), t.object.clone())
                };
                self.navigator.update(cx, |n, cx| n.reveal(&conn_id, &object, cx));
                let h = t.read(cx).grid_focus(cx);
                h.focus(window, cx);
            }
            Tab::Console(c) => {
                let h = c.read(cx).editor_focus(cx);
                h.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        if let Tab::Table(t) = &self.tabs[ix]
            && t.read(cx).has_changes(cx) {
                let t = t.clone();
                let id = self.tabs[ix].entity_id();
                let ws = cx.entity().downgrade();
                window.open_dialog(cx, move |dialog, _, _| {
                    let (t, ws) = (t.clone(), ws.clone());
                    dialog
                        .title("Unsaved changes")
                        .child("This tab has staged changes that haven't been saved.")
                        .footer(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .gap_2()
                                .child(Button::new("keep").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                                .child(Button::new("discard").danger().label("Discard & close").on_click(move |_, w, cx| {
                                    w.close_dialog(cx);
                                    t.update(cx, |t, cx| t.discard(w, cx));
                                    ws.update(cx, |ws, cx| ws.remove_tab(id, w, cx)).ok();
                                })),
                        )
                });
                return;
            }
        let id = self.tabs[ix].entity_id();
        self.remove_tab(id, window, cx);
    }

    pub fn remove_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|t| t.entity_id() == id) else { return };
        self.tabs.remove(ix);
        if self.tabs.is_empty() {
            self.active = 0;
            self.focus.focus(window, cx);
        } else {
            let next = if self.active > ix || self.active >= self.tabs.len() {
                self.active.saturating_sub(1)
            } else {
                self.active
            };
            self.activate(next.min(self.tabs.len() - 1), window, cx);
        }
        cx.notify();
    }

    /// Console for the active tab's connection (or the first connected one).
    fn new_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.tabs.get(self.active) {
            Some(Tab::Table(t)) => {
                let t = t.read(cx);
                let coll = (t.kind() == DbKind::Mongo).then(|| t.object.name.clone());
                Some((t.conn_id.clone(), t.object.database.clone(), coll))
            }
            Some(Tab::Console(c)) => {
                let c = c.read(cx);
                Some((c.conn_id.clone(), c.database.clone(), c.collection.clone()))
            }
            None => {
                let state = self.state.read(cx);
                state
                    .connections
                    .iter()
                    .find_map(|c| state.connection(&c.id).map(|conn| (c.id.clone(), conn.default_database().unwrap_or_default(), None)))
            }
        };
        match target {
            Some((id, db, coll)) => self.open_console(&id, db, coll, window, cx),
            None => window.push_notification(Notification::info("Connect to a database first"), cx),
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        h_flex()
            .id("tabs")
            .h(px(34.))
            .bg(theme.tab_bar)
            .border_b_1()
            .border_color(theme.border)
            .overflow_x_scroll()
            .children(self.tabs.iter().enumerate().map(|(ix, tab)| {
                let active = ix == self.active;
                let (icon, title, dirty, running, sub) = match tab {
                    Tab::Table(t) => {
                        let t = t.read(cx);
                        let icon = match t.object.kind {
                            ObjectKind::Collection => Lucide::Layers,
                            ObjectKind::View | ObjectKind::MaterializedView | ObjectKind::MongoView => Lucide::Eye,
                            _ => Lucide::Table2,
                        };
                        (icon, t.title(), t.has_changes(cx), false, t.conn_name.clone())
                    }
                    Tab::Console(c) => {
                        let c = c.read(cx);
                        (Lucide::SquareTerminal, c.title(), false, c.is_running(), c.conn_name.clone())
                    }
                };
                h_flex()
                    .id(("tab", ix))
                    .group("tab")
                    .h_full()
                    .pl_3()
                    .pr_1()
                    .gap_1p5()
                    .max_w(px(240.))
                    .flex_shrink_0()
                    .text_sm()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(active, |this| this.bg(theme.tab_active).text_color(theme.tab_active_foreground))
                    .when(!active, |this| this.text_color(theme.tab_foreground).hover(|s| s.bg(theme.list_hover)))
                    .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                    .child(div().truncate().child(title))
                    .when(dirty, |this| this.child(div().size(px(7.)).rounded_full().bg(theme.warning)))
                    .when(running, |this| {
                        this.child(Icon::new(Lucide::LoaderCircle).xsmall().text_color(theme.muted_foreground))
                    })
                    .child(
                        Button::new(("close", ix))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::X))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(ix, window, cx)
                            })),
                    )
                    .tooltip(move |window, cx| Tooltip::new(sub.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
            }))
            .child(
                div().px_1().child(
                    Button::new("new-console")
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(Lucide::Plus))
                        .tooltip("New console (⌘T)")
                        .on_click(cx.listener(|this, _, window, cx| this.new_console(window, cx))),
                ),
            )
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let has_connections = !self.state.read(cx).connections.is_empty();
        let shortcut = |keys: &'static str, what: &'static str| {
            h_flex()
                .gap_3()
                .child(div().w(px(70.)).text_right().font_family(theme.mono_font_family.clone()).child(keys))
                .child(div().text_color(theme.muted_foreground).child(what))
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child(Icon::new(Lucide::Database).size(px(48.)).text_color(theme.muted_foreground.opacity(0.5)))
            .child(div().text_xl().font_semibold().child("DB Claw"))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if has_connections {
                        "Pick a table or collection in the sidebar."
                    } else {
                        "Add a PostgreSQL or MongoDB connection to get started."
                    }),
            )
            .when(!has_connections, |this| {
                this.child(
                    Button::new("welcome-new")
                        .primary()
                        .icon(Icon::new(Lucide::Plus))
                        .label("New connection")
                        .on_click(cx.listener(|this, _, window, cx| {
                            connection_dialog::open(None, this.state.clone(), window, cx)
                        })),
                )
            })
            .child(
                v_flex()
                    .mt_4()
                    .gap_1()
                    .text_sm()
                    .child(shortcut("⌘N", "New connection"))
                    .child(shortcut("⌘T", "New query console"))
                    .child(shortcut("⌘P", "Search tables"))
                    .child(shortcut("⌘F", "Filter the current table"))
                    .child(shortcut("⌘↵", "Run query"))
                    .child(shortcut("⌘S", "Save staged changes"))
                    .child(shortcut("⌘B", "Toggle sidebar")),
            )
    }
}

// ---- automation API ----------------------------------------------------------

impl Workspace {
    pub fn state(&self) -> &Entity<AppState> {
        &self.state
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn tab_id(tab: &Tab) -> u64 {
        tab.entity_id().as_u64()
    }

    pub fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.entity_id().as_u64() == id)
    }

    pub fn find_table(&self, conn_id: &str, object: &ObjectRef, cx: &App) -> Option<Entity<TableView>> {
        self.tabs.iter().find_map(|t| match t {
            Tab::Table(v) if v.read(cx).conn_id == conn_id && same_table(&v.read(cx).object, object) => Some(v.clone()),
            _ => None,
        })
    }

    pub fn last_console(&self) -> Option<Entity<ConsoleView>> {
        self.tabs.iter().rev().find_map(|t| match t {
            Tab::Console(c) => Some(c.clone()),
            _ => None,
        })
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content: AnyElement = match self.tabs.get(self.active) {
            Some(Tab::Table(t)) => t.clone().into_any_element(),
            Some(Tab::Console(c)) => c.clone().into_any_element(),
            None => self.render_welcome(cx).into_any_element(),
        };
        let main = v_flex()
            .size_full()
            .when(!self.tabs.is_empty(), |this| this.child(self.render_tabs(cx)))
            .child(div().flex_1().min_h_0().child(content));
        let _ = window;

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &NewConnection, window, cx| {
                connection_dialog::open(None, this.state.clone(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewConsole, window, cx| this.new_console(window, cx)))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let ix = this.active;
                this.close_tab(ix, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                if !this.tabs.is_empty() {
                    let ix = (this.active + 1) % this.tabs.len();
                    this.activate(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PrevTab, window, cx| {
                if !this.tabs.is_empty() {
                    let ix = (this.active + this.tabs.len() - 1) % this.tabs.len();
                    this.activate(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar = !this.sidebar;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &SearchTables, window, cx| {
                this.sidebar = true;
                let h = this.navigator.read(cx).search_focus(cx);
                h.focus(window, cx);
                cx.notify();
            }))
            .child(if self.sidebar {
                h_resizable("main-split")
                    .child(resizable_panel().size(px(270.)).size_range(px(180.)..px(520.)).child(self.navigator.clone()))
                    .child(resizable_panel().child(main))
                    .into_any_element()
            } else {
                main.into_any_element()
            })
    }
}
