//! Sidebar: connections → databases → (schemas →) tables / collections.
//! A flattened, virtualized tree with lazy loading.

use std::collections::{HashMap, HashSet};

use dbclaw_core::model::{DbKind, ObjectInfo, ObjectKind, ObjectRef};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::drag::{DraggedConnection, drop_marker};
use crate::state::{AppEvent, AppState, LiveStatus};
use crate::util::{group_digits, io};

pub enum NavEvent {
    OpenObject { conn_id: String, object: ObjectRef },
    OpenConsole { conn_id: String, database: String, collection: Option<String> },
    NewConnection,
    EditConnection(String),
    DeleteConnection(String),
}

#[derive(Clone)]
enum Load<T> {
    Loading,
    Loaded(T),
    Failed(SharedString),
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Conn(String),
    Db(String, String),
    Schema(String, String, String),
}

#[derive(Clone)]
enum RowKind {
    Conn { id: String, name: SharedString, kind: DbKind },
    Db { conn: String, db: String },
    Schema { conn: String, db: String, schema: String, count: usize },
    Object { conn: String, info: ObjectInfo },
    Info { text: SharedString, error: bool },
}

#[derive(Clone)]
struct Row {
    depth: usize,
    kind: RowKind,
}

pub struct Navigator {
    state: Entity<AppState>,
    databases: HashMap<String, Load<Vec<String>>>,
    objects: HashMap<(String, String), Load<Vec<ObjectInfo>>>,
    expanded: HashSet<Key>,
    rows: Vec<Row>,
    selected: Option<Key>,
    selected_object: Option<(String, ObjectRef)>,
    search: Entity<InputState>,
    scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NavEvent> for Navigator {}

impl Navigator {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search tables…"));
        let subs = vec![
            cx.subscribe(&state, |this, _, ev: &AppEvent, cx| this.on_app_event(ev, cx)),
            cx.subscribe(&search, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.rebuild(cx);
                }
            }),
        ];
        let mut this = Self {
            state,
            databases: HashMap::new(),
            objects: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            selected_object: None,
            search,
            scroll: UniformListScrollHandle::new(),
            _subscriptions: subs,
        };
        this.rebuild(cx);
        this
    }

    pub fn search_focus(&self, cx: &App) -> FocusHandle {
        self.search.focus_handle(cx)
    }

    fn on_app_event(&mut self, ev: &AppEvent, cx: &mut Context<Self>) {
        match ev {
            AppEvent::ConnectionsChanged => {}
            AppEvent::StatusChanged(id) => {
                let status = self.state.read(cx).status(id).cloned();
                match status {
                    Some(LiveStatus::Connected(conn)) => {
                        self.expanded.insert(Key::Conn(id.clone()));
                        if !self.databases.contains_key(id) {
                            self.load_databases(id.clone(), conn.default_database(), cx);
                        }
                    }
                    None => {
                        // Disconnected or edited: forget cached metadata.
                        self.databases.remove(id);
                        self.objects.retain(|(c, _), _| c != id);
                    }
                    _ => {}
                }
            }
        }
        self.rebuild(cx);
    }

    /// Mark a table as the selected one (e.g. when its tab is activated).
    pub fn reveal(&mut self, conn_id: &str, object: &ObjectRef, cx: &mut Context<Self>) {
        self.selected_object = Some((conn_id.to_string(), object.clone()));
        cx.notify();
    }

    fn load_databases(&mut self, conn_id: String, default_db: Option<String>, cx: &mut Context<Self>) {
        let Some(conn) = self.state.read(cx).connection(&conn_id) else { return };
        self.databases.insert(conn_id.clone(), Load::Loading);
        cx.spawn(async move |this, cx| {
            let result = io(async move { conn.list_databases().await }).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(dbs) => {
                        // Auto-expand the database from the URL (or the only one).
                        let auto = default_db
                            .filter(|d| dbs.contains(d))
                            .or_else(|| (dbs.len() == 1).then(|| dbs[0].clone()));
                        this.databases.insert(conn_id.clone(), Load::Loaded(dbs));
                        if let Some(db) = auto {
                            this.expand_db(conn_id.clone(), db, cx);
                        }
                    }
                    Err(e) => {
                        this.databases.insert(
                            conn_id,
                            Load::Failed(dbclaw_drivers::error_message(&e).into()),
                        );
                    }
                }
                this.rebuild(cx);
            })
            .ok();
        })
        .detach();
        self.rebuild(cx);
    }

    fn expand_db(&mut self, conn_id: String, db: String, cx: &mut Context<Self>) {
        self.expanded.insert(Key::Db(conn_id.clone(), db.clone()));
        if !matches!(self.objects.get(&(conn_id.clone(), db.clone())), Some(Load::Loaded(_) | Load::Loading)) {
            self.load_objects(conn_id, db, cx);
        }
    }

    fn load_objects(&mut self, conn_id: String, db: String, cx: &mut Context<Self>) {
        let Some(conn) = self.state.read(cx).connection(&conn_id) else { return };
        let key = (conn_id.clone(), db.clone());
        self.objects.insert(key.clone(), Load::Loading);
        let db2 = db.clone();
        cx.spawn(async move |this, cx| {
            let result = io(async move { conn.list_objects(&db2).await }).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(objs) => {
                        // Expand "public" (or the only schema) by default.
                        let schemas: HashSet<&str> =
                            objs.iter().map(|o| o.object.schema.as_str()).collect();
                        let auto = if schemas.contains("public") {
                            Some("public".to_string())
                        } else if schemas.len() == 1 {
                            schemas.iter().next().map(|s| s.to_string())
                        } else {
                            None
                        };
                        if let Some(s) = auto.filter(|s| !s.is_empty()) {
                            this.expanded.insert(Key::Schema(conn_id.clone(), db.clone(), s));
                        }
                        this.objects.insert(key, Load::Loaded(objs));
                    }
                    Err(e) => {
                        this.objects.insert(key, Load::Failed(dbclaw_drivers::error_message(&e).into()));
                    }
                }
                this.rebuild(cx);
            })
            .ok();
        })
        .detach();
        self.rebuild(cx);
    }

    /// Reload metadata of a connection (databases and expanded objects).
    pub fn refresh_connection(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        let expanded_dbs: Vec<String> = self
            .expanded
            .iter()
            .filter_map(|k| match k {
                Key::Db(c, d) if c == conn_id => Some(d.clone()),
                _ => None,
            })
            .collect();
        self.objects.retain(|(c, _), _| c != conn_id);
        if let Some(conn) = self.state.read(cx).connection(conn_id) {
            self.load_databases(conn_id.to_string(), conn.default_database(), cx);
            for db in expanded_dbs {
                self.load_objects(conn_id.to_string(), db, cx);
            }
        }
    }

    pub fn refresh_database(&mut self, conn_id: &str, db: &str, cx: &mut Context<Self>) {
        self.load_objects(conn_id.to_string(), db.to_string(), cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let searching = !query.is_empty();
        let state = self.state.read(cx);
        let mut rows = Vec::new();
        for c in &state.connections {
            rows.push(Row {
                depth: 0,
                kind: RowKind::Conn { id: c.id.clone(), name: c.name.clone().into(), kind: c.kind },
            });
            if !self.expanded.contains(&Key::Conn(c.id.clone())) {
                continue;
            }
            match state.status(&c.id) {
                None => continue,
                Some(LiveStatus::Connecting) => {
                    rows.push(info_row(1, "Connecting…", false));
                    continue;
                }
                Some(LiveStatus::Failed(e)) => {
                    rows.push(info_row(1, e.clone(), true));
                    continue;
                }
                Some(LiveStatus::Connected(_)) => {}
            }
            match self.databases.get(&c.id) {
                None | Some(Load::Loading) => rows.push(info_row(1, "Loading…", false)),
                Some(Load::Failed(e)) => rows.push(info_row(1, e.clone(), true)),
                Some(Load::Loaded(dbs)) => {
                    for db in dbs {
                        let key = Key::Db(c.id.clone(), db.clone());
                        let objs = self.objects.get(&(c.id.clone(), db.clone()));
                        if searching && !matches!(objs, Some(Load::Loaded(_))) {
                            continue;
                        }
                        let db_row = Row { depth: 1, kind: RowKind::Db { conn: c.id.clone(), db: db.clone() } };
                        if !searching && !self.expanded.contains(&key) {
                            rows.push(db_row);
                            continue;
                        }
                        let mut children = Vec::new();
                        match objs {
                            None | Some(Load::Loading) => children.push(info_row(2, "Loading…", false)),
                            Some(Load::Failed(e)) => children.push(info_row(2, e.clone(), true)),
                            Some(Load::Loaded(list)) => {
                                let matching: Vec<&ObjectInfo> = list
                                    .iter()
                                    .filter(|o| !searching || o.object.name.to_lowercase().contains(&query))
                                    .collect();
                                if c.kind == DbKind::Mongo {
                                    for o in matching {
                                        children.push(Row {
                                            depth: 2,
                                            kind: RowKind::Object { conn: c.id.clone(), info: o.clone() },
                                        });
                                    }
                                } else {
                                    let mut i = 0;
                                    while i < matching.len() {
                                        let schema = matching[i].object.schema.clone();
                                        let group: Vec<&ObjectInfo> = matching[i..]
                                            .iter()
                                            .take_while(|o| o.object.schema == schema)
                                            .copied()
                                            .collect();
                                        i += group.len();
                                        let skey = Key::Schema(c.id.clone(), db.clone(), schema.clone());
                                        children.push(Row {
                                            depth: 2,
                                            kind: RowKind::Schema {
                                                conn: c.id.clone(),
                                                db: db.clone(),
                                                schema: schema.clone(),
                                                count: group.len(),
                                            },
                                        });
                                        if searching || self.expanded.contains(&skey) {
                                            for o in group {
                                                children.push(Row {
                                                    depth: 3,
                                                    kind: RowKind::Object { conn: c.id.clone(), info: o.clone() },
                                                });
                                            }
                                        }
                                    }
                                }
                                if list.is_empty() {
                                    children.push(info_row(2, "No tables", false));
                                }
                            }
                        }
                        if searching && !children.iter().any(|r| matches!(r.kind, RowKind::Object { .. })) {
                            continue;
                        }
                        rows.push(db_row);
                        rows.extend(children);
                    }
                }
            }
        }
        self.rows = rows;
        cx.notify();
    }

    fn click_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).cloned() else { return };
        match row.kind {
            RowKind::Conn { id, .. } => {
                let key = Key::Conn(id.clone());
                self.selected = Some(key.clone());
                let status = self.state.read(cx).status(&id).cloned();
                match status {
                    None | Some(LiveStatus::Failed(_)) => {
                        self.expanded.insert(key);
                        self.state.update(cx, |s, cx| {
                            s.live.remove(&id);
                            s.connect(&id, cx)
                        });
                    }
                    _ => {
                        if !self.expanded.remove(&key) {
                            self.expanded.insert(key);
                        }
                    }
                }
            }
            RowKind::Db { conn, db } => {
                let key = Key::Db(conn.clone(), db.clone());
                self.selected = Some(key.clone());
                if !self.expanded.remove(&key) {
                    self.expand_db(conn, db, cx);
                }
            }
            RowKind::Schema { conn, db, schema, .. } => {
                let key = Key::Schema(conn, db, schema);
                self.selected = Some(key.clone());
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
            RowKind::Object { conn, info } => {
                self.selected = None;
                self.selected_object = Some((conn.clone(), info.object.clone()));
                cx.emit(NavEvent::OpenObject { conn_id: conn, object: info.object });
            }
            RowKind::Info { .. } => {}
        }
        let _ = window;
        self.rebuild(cx);
    }

    fn render_row(&self, ix: usize, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[ix];
        let theme = cx.theme();
        let indent = px(8.0 + row.depth as f32 * 14.0);
        let (chevron, icon, icon_color, label, trailing, is_selected): (
            Option<bool>,
            Option<Lucide>,
            Option<Hsla>,
            SharedString,
            Option<SharedString>,
            bool,
        ) = match &row.kind {
            RowKind::Conn { id, name, kind } => {
                let status = self.state.read(cx).status(id);
                let color = match status {
                    Some(LiveStatus::Connected(_)) => theme.success,
                    Some(LiveStatus::Failed(_)) => theme.danger,
                    Some(LiveStatus::Connecting) => theme.warning,
                    None => theme.muted_foreground,
                };
                let icon = match kind {
                    DbKind::Postgres => Lucide::Database,
                    DbKind::Mongo => Lucide::Leaf,
                };
                (
                    Some(self.expanded.contains(&Key::Conn(id.clone()))),
                    Some(icon),
                    Some(color),
                    name.clone(),
                    None,
                    self.selected == Some(Key::Conn(id.clone())),
                )
            }
            RowKind::Db { conn, db } => (
                Some(self.expanded.contains(&Key::Db(conn.clone(), db.clone()))),
                Some(Lucide::Database),
                Some(theme.muted_foreground),
                db.clone().into(),
                None,
                self.selected == Some(Key::Db(conn.clone(), db.clone())),
            ),
            RowKind::Schema { conn, db, schema, count } => (
                Some(self.expanded.contains(&Key::Schema(conn.clone(), db.clone(), schema.clone()))),
                Some(Lucide::Folder),
                Some(theme.muted_foreground),
                schema.clone().into(),
                Some(count.to_string().into()),
                self.selected == Some(Key::Schema(conn.clone(), db.clone(), schema.clone())),
            ),
            RowKind::Object { conn, info } => {
                let (icon, color) = match info.object.kind {
                    ObjectKind::View | ObjectKind::MaterializedView | ObjectKind::MongoView => {
                        (Lucide::Eye, theme.info)
                    }
                    ObjectKind::Collection => (Lucide::Layers, theme.success),
                    _ => (Lucide::Table2, theme.primary),
                };
                let rows = info
                    .estimated_rows
                    .filter(|n| *n > 0)
                    .map(|n| SharedString::from(format!("~{}", group_digits(n as u64))));
                let sel = self
                    .selected_object
                    .as_ref()
                    .is_some_and(|(c, o)| c == conn && *o == info.object);
                (None, Some(icon), Some(color), info.object.name.clone().into(), rows, sel)
            }
            RowKind::Info { text, error } => {
                return h_flex()
                    .id(("nav-row", ix))
                    .h(px(24.))
                    .pl(indent + px(16.))
                    .pr_2()
                    .text_xs()
                    .text_color(if *error { theme.danger } else { theme.muted_foreground })
                    .child(div().truncate().child(text.clone()))
                    .when(*error, |this| {
                        let text = text.clone();
                        this.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                    })
                    .into_any_element();
            }
        };

        // Colour tag of the row's connection: a bar on the left edge of the connection
        // and everything under it, so you can tell e.g. production apart at a glance.
        let tag_of = match &row.kind {
            RowKind::Conn { id, .. } => Some((id.as_str(), None)),
            RowKind::Db { conn, db } | RowKind::Schema { conn, db, .. } => Some((conn.as_str(), Some(db.as_str()))),
            RowKind::Object { conn, info } => Some((conn.as_str(), Some(info.object.database.as_str()))),
            RowKind::Info { .. } => None,
        };
        let tag = tag_of.and_then(|(id, db)| crate::conn_color::of(&self.state, id, db, cx));
        let theme = cx.theme();
        let el = h_flex()
            .id(("nav-row", ix))
            .relative()
            .when_some(tag, |this, color| {
                this.child(div().absolute().left_0().top_0().bottom_0().w(px(3.)).bg(color))
            })
            .h(px(24.))
            .pl(indent)
            .pr_2()
            .gap_1p5()
            .text_sm()
            .rounded(theme.radius)
            .cursor_pointer()
            .when(is_selected, |this| this.bg(theme.list_active))
            .hover(|this| this.bg(theme.list_hover))
            .child(
                div().w(px(12.)).flex_shrink_0().when_some(chevron, |this, open| {
                    this.child(
                        Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight })
                            .xsmall()
                            .text_color(theme.muted_foreground),
                    )
                }),
            )
            .when_some(icon, |this, icon| {
                this.child(
                    Icon::new(icon)
                        .small()
                        .when_some(icon_color, |i, c| i.text_color(c)),
                )
            })
            .child(div().flex_1().truncate().child(label))
            .when_some(trailing, |this, t| {
                this.child(div().text_xs().text_color(theme.muted_foreground).child(t))
            })
            .on_click(cx.listener(move |this, _, window, cx| this.click_row(ix, window, cx)));

        // Connections reorder by drag and drop; a bar marks where the dragged one lands.
        let el = match &row.kind {
            RowKind::Conn { id, name, kind } => {
                let pos = self.state.read(cx).connections.iter().position(|c| &c.id == id).unwrap_or(0);
                let dragged = DraggedConnection {
                    id: id.clone(),
                    ix: pos,
                    icon: match kind {
                        DbKind::Postgres => Lucide::Database,
                        DbKind::Mongo => Lucide::Leaf,
                    },
                    name: name.clone(),
                    grab: Point::default(),
                };
                let target = id.clone();
                el.on_drag(dragged, |d, grab, _, cx| cx.new(|_| DraggedConnection { grab, ..d.clone() }))
                    .drag_over::<DraggedConnection>(move |style, d, _, cx| drop_marker(style, d.ix, pos, false, cx))
                    .on_drop(cx.listener(move |this, d: &DraggedConnection, _, cx| {
                        let target = target.clone();
                        this.state.update(cx, |s, cx| s.move_connection(&d.id, &target, cx));
                    }))
            }
            _ => el,
        };

        let kind = row.kind.clone();
        let view = cx.entity().downgrade();
        el.context_menu(move |menu, window, cx| {
            let view = view.clone();
            build_menu(menu, &kind, view, window, cx)
        })
        .into_any_element()
    }
}

type NavAction = Box<dyn Fn(&mut Navigator, &mut Window, &mut Context<Navigator>)>;

fn info_row(depth: usize, text: impl Into<SharedString>, error: bool) -> Row {
    Row { depth, kind: RowKind::Info { text: text.into(), error } }
}

/// "Color" submenu: the colour tag of a connection (`database` = `None`) or of one of
/// its databases (which can also follow the connection's colour).
fn color_submenu(
    menu: gpui_kit::component::menu::PopupMenu,
    view: WeakEntity<Navigator>,
    conn: String,
    database: Option<String>,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let current = view.upgrade().and_then(|v| {
        let state = v.read(cx).state.read(cx);
        let c = state.config(&conn)?;
        match &database {
            None => c.color.clone(),
            Some(db) => c.database_colors.get(db).cloned(),
        }
    });
    menu.submenu_with_icon(Some(Icon::new(Lucide::Palette)), "Color", window, cx, move |sub, _, cx| {
        let set = |value: Option<&'static str>| {
            let (view, conn, database) = (view.clone(), conn.clone(), database.clone());
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                view.update(cx, |this, cx| {
                    this.state.update(cx, |s, cx| s.set_color(&conn, database.as_deref(), value.map(str::to_string), cx))
                })
                .ok();
            }
        };
        let none_label = if database.is_some() { "Same as connection" } else { "None" };
        // The menu draws no check mark next to an icon, so the current choice shows a
        // checked circle instead.
        let mark = |selected: bool| Icon::new(if selected { Lucide::CircleCheck } else { Lucide::Circle });
        let mut sub = sub.item(PopupMenuItem::new(none_label).icon(mark(current.is_none())).on_click(set(None)));
        for (name, label) in crate::conn_color::PRESETS {
            let mut icon = mark(current.as_deref() == Some(*name));
            if let Some(c) = crate::conn_color::resolve(Some(name), cx) {
                icon = icon.text_color(c);
            }
            sub = sub.item(
                PopupMenuItem::new(*label).icon(icon).on_click(set(Some(name))),
            );
        }
        sub
    })
}

fn build_menu(
    menu: gpui_kit::component::menu::PopupMenu,
    kind: &RowKind,
    view: WeakEntity<Navigator>,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let color_view = view.clone();
    let item = |label: &'static str, icon: Lucide, f: NavAction| {
        let view = view.clone();
        let f = std::rc::Rc::new(f);
        PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
            let f = f.clone();
            view.update(cx, |this, cx| f(this, window, cx)).ok();
        })
    };
    match kind.clone() {
        RowKind::Conn { id, .. } => {
            let (a, b, c, d, e) = (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
            menu.item(item("New console", Lucide::SquareTerminal, Box::new(move |this, _, cx| {
                let conn = this.state.read(cx).connection(&a);
                let db = conn.and_then(|c| c.default_database()).unwrap_or_default();
                if this.state.read(cx).connection(&a).is_some() {
                    cx.emit(NavEvent::OpenConsole { conn_id: a.clone(), database: db, collection: None });
                }
            })))
            .item(item("Refresh", Lucide::RefreshCw, Box::new(move |this, _, cx| this.refresh_connection(&b, cx))))
            .separator()
            .item(item("Edit connection…", Lucide::Pencil, Box::new(move |_, _, cx| cx.emit(NavEvent::EditConnection(c.clone())))))
            .map(|m| color_submenu(m, color_view.clone(), id.clone(), None, window, cx))
            .item(item("Disconnect", Lucide::Unplug, Box::new(move |this, _, cx| {
                this.state.update(cx, |s, cx| s.disconnect(&d, cx));
            })))
            .separator()
            .item(item("Delete connection", Lucide::Trash, Box::new(move |_, _, cx| cx.emit(NavEvent::DeleteConnection(e.clone())))))
        }
        RowKind::Db { conn, db } => {
            let (c3, d3) = (conn.clone(), db.clone());
            let (c1, d1, c2, d2) = (conn.clone(), db.clone(), conn, db);
            menu.item(item("New console", Lucide::SquareTerminal, Box::new(move |_, _, cx| {
                cx.emit(NavEvent::OpenConsole { conn_id: c1.clone(), database: d1.clone(), collection: None })
            })))
            .item(item("Refresh", Lucide::RefreshCw, Box::new(move |this, _, cx| this.refresh_database(&c2, &d2, cx))))
            .separator()
            .map(|m| color_submenu(m, color_view.clone(), c3, Some(d3), window, cx))
        }
        RowKind::Schema { conn, db, .. } => {
            let (c1, d1) = (conn, db);
            menu.item(item("New console", Lucide::SquareTerminal, Box::new(move |_, _, cx| {
                cx.emit(NavEvent::OpenConsole { conn_id: c1.clone(), database: d1.clone(), collection: None })
            })))
        }
        RowKind::Object { conn, info } => {
            let (c1, o1) = (conn.clone(), info.object.clone());
            let (c2, o2) = (conn, info.object.clone());
            let name = info.object.display_name();
            menu.item(item("Open", Lucide::Table2, Box::new(move |_, _, cx| {
                cx.emit(NavEvent::OpenObject { conn_id: c1.clone(), object: o1.clone() })
            })))
            .item(item("New console here", Lucide::SquareTerminal, Box::new(move |_, _, cx| {
                let collection = (o2.schema.is_empty()).then(|| o2.name.clone());
                cx.emit(NavEvent::OpenConsole { conn_id: c2.clone(), database: o2.database.clone(), collection })
            })))
            .item(item("Copy name", Lucide::Copy, Box::new(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(name.clone()));
            })))
        }
        RowKind::Info { .. } => menu,
    }
}

impl Render for Navigator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let empty = self.rows.is_empty();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(Input::new(&self.search).small().cleanable(true))
                    .child(
                        Button::new("new-conn")
                            .icon(Icon::new(Lucide::Plus))
                            .ghost()
                            .small()
                            .tooltip("New connection (⌘N)")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::NewConnection))),
                    ),
            )
            .child(if empty {
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_4()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No connections yet")
                    .child(
                        Button::new("first-conn")
                            .primary()
                            .small()
                            .label("New connection")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NavEvent::NewConnection))),
                    )
                    .into_any_element()
            } else {
                uniform_list(
                    "nav-list",
                    self.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        range.map(|ix| this.render_row(ix, window, cx)).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1()
                .p_1()
                .into_any_element()
            })
    }
}
