//! New / edit connection dialog. Connections can be entered as individual
//! fields (host, port, database, user, password, options) or as a URL;
//! switching converts between the two. Either way the connection is stored
//! as a URL plus a separate password.

use dbclaw_core::conn_url::{self, UrlParts};
use dbclaw_core::model::{ConnectionConfig, DbKind, split_url_password};
use dbclaw_drivers::Connection;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::state::AppState;
use crate::util::io;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Fields,
    Url,
}

pub struct ConnectionForm {
    id: Option<String>,
    kind: DbKind,
    mode: Mode,
    srv: bool,
    name: Entity<InputState>,
    url: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    database: Entity<InputState>,
    user: Entity<InputState>,
    options: Entity<InputState>,
    password: Entity<InputState>,
    read_only: bool,
    show_all_databases: bool,
    /// Colour tag preset (see `conn_color`).
    color: Option<String>,
    /// Per-database colours (set from the sidebar), kept as they are.
    database_colors: std::collections::BTreeMap<String, String>,
    testing: bool,
    test_result: Option<Result<String, String>>,
    name_touched: bool,
    _subs: Vec<Subscription>,
}

fn options_placeholder(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Postgres => "e.g. sslmode=require&connect_timeout=5",
        DbKind::Mongo => "e.g. authSource=admin&tls=true&replicaSet=rs0",
    }
}

fn port_placeholder(kind: DbKind) -> String {
    conn_url::default_port(kind).to_string()
}

impl ConnectionForm {
    fn new(existing: Option<ConnectionConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = existing.as_ref().map_or(DbKind::Postgres, |c| c.kind);
        let url_text = existing.as_ref().map(|c| c.url.clone()).unwrap_or_default();
        // Open in field mode unless the saved URL can't be split into fields.
        let parsed = (!url_text.is_empty()).then(|| conn_url::parse(&url_text).ok()).flatten();
        let mode = if url_text.is_empty() || parsed.is_some() { Mode::Fields } else { Mode::Url };
        let p = parsed.map(|(_, p)| p).unwrap_or_default();

        let input = |placeholder: String, value: String, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        let name = input(
            "My database".into(),
            existing.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
            window,
            cx,
        );
        let url = input(kind.default_url().into(), url_text, window, cx);
        let host = input("localhost".into(), p.host.clone(), window, cx);
        let port = input(port_placeholder(kind), p.port.map(|p| p.to_string()).unwrap_or_default(), window, cx);
        let database = input(
            match kind {
                DbKind::Postgres => "optional · defaults to the user name".into(),
                DbKind::Mongo => "optional · opened first in the sidebar".into(),
            },
            p.database.clone(),
            window,
            cx,
        );
        let user = input(
            std::env::var("USER").unwrap_or_else(|_| "user".into()),
            p.user.clone(),
            window,
            cx,
        );
        let options = input(options_placeholder(kind).into(), p.options.clone(), window, cx);
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("optional")
                .default_value(existing.as_ref().and_then(|c| c.password.clone()).unwrap_or_default())
        });

        let mut subs = vec![
            cx.subscribe_in(&url, window, |this, url, ev: &InputEvent, window, cx| {
                if !matches!(ev, InputEvent::Change) {
                    return;
                }
                let text = url.read(cx).value().to_string();
                if let Some(kind) = DbKind::from_url(&text) {
                    this.set_kind(kind, window, cx);
                }
                // Move a pasted password into the password field.
                let (without, pw) = split_url_password(&text);
                if let Some(pw) = pw {
                    url.update(cx, |s, cx| s.set_value(without.clone(), window, cx));
                    this.password.update(cx, |s, cx| s.set_value(pw, window, cx));
                }
                this.after_edit(window, cx);
            }),
            cx.subscribe(&name, |this, _, ev: &InputEvent, _| {
                if matches!(ev, InputEvent::Change) {
                    this.name_touched = true;
                }
            }),
        ];
        for field in [&host, &port, &database, &user, &options] {
            subs.push(cx.subscribe_in(field, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.after_edit(window, cx);
                }
            }));
        }
        // A full URL pasted into the host field is split into the fields.
        subs.push(cx.subscribe_in(&host, window, |this, host, ev: &InputEvent, window, cx| {
            if !matches!(ev, InputEvent::Change) {
                return;
            }
            let text = host.read(cx).value().to_string();
            if text.contains("://") {
                let (without, pw) = split_url_password(&text);
                if let Ok((kind, parts)) = conn_url::parse(&without) {
                    this.set_kind(kind, window, cx);
                    this.fill_fields(&parts, window, cx);
                    if let Some(pw) = pw {
                        this.password.update(cx, |s, cx| s.set_value(pw, window, cx));
                    }
                    this.after_edit(window, cx);
                }
            }
        }));

        Self {
            name_touched: existing.is_some(),
            id: existing.as_ref().map(|c| c.id.clone()),
            kind,
            mode,
            srv: p.srv,
            name,
            url,
            host,
            port,
            database,
            user,
            options,
            password,
            read_only: existing.as_ref().is_some_and(|c| c.read_only),
            show_all_databases: existing.as_ref().is_some_and(|c| c.show_all_databases),
            color: existing.as_ref().and_then(|c| c.color.clone()),
            database_colors: existing.as_ref().map(|c| c.database_colors.clone()).unwrap_or_default(),
            testing: false,
            test_result: None,
            _subs: subs,
        }
    }

    fn initial_focus(&self) -> Entity<InputState> {
        match self.mode {
            Mode::Fields => self.host.clone(),
            Mode::Url => self.url.clone(),
        }
    }

    fn set_kind(&mut self, kind: DbKind, window: &mut Window, cx: &mut Context<Self>) {
        if kind == self.kind {
            return;
        }
        let old_default = conn_url::default_port(self.kind).to_string();
        self.kind = kind;
        if kind != DbKind::Mongo {
            self.srv = false;
        }
        // Swap a default port for the other database's default.
        if self.port.read(cx).value().trim() == old_default {
            self.port.update(cx, |s, cx| s.set_value(port_placeholder(kind), window, cx));
        }
        self.url.update(cx, |s, cx| s.set_placeholder(kind.default_url(), window, cx));
        self.port.update(cx, |s, cx| s.set_placeholder(port_placeholder(kind), window, cx));
        self.options.update(cx, |s, cx| s.set_placeholder(options_placeholder(kind), window, cx));
        self.database.update(cx, |s, cx| {
            s.set_placeholder(
                match kind {
                    DbKind::Postgres => "optional · defaults to the user name",
                    DbKind::Mongo => "optional · opened first in the sidebar",
                },
                window,
                cx,
            )
        });
        self.test_result = None;
        cx.notify();
    }

    fn fill_fields(&mut self, p: &UrlParts, window: &mut Window, cx: &mut Context<Self>) {
        self.srv = p.srv;
        let set = |input: &Entity<InputState>, v: String, window: &mut Window, cx: &mut Context<Self>| {
            input.update(cx, |s, cx| s.set_value(v, window, cx));
        };
        set(&self.host, p.host.clone(), window, cx);
        set(&self.port, p.port.map(|p| p.to_string()).unwrap_or_default(), window, cx);
        set(&self.database, p.database.clone(), window, cx);
        set(&self.user, p.user.clone(), window, cx);
        set(&self.options, p.options.clone(), window, cx);
    }

    fn parts(&self, cx: &App) -> Result<UrlParts, String> {
        let port_text = self.port.read(cx).value().trim().to_string();
        let port = if port_text.is_empty() || self.srv {
            None
        } else {
            Some(port_text.parse::<u16>().map_err(|_| format!("Port must be a number, got “{port_text}”"))?)
        };
        Ok(UrlParts {
            srv: self.srv && self.kind == DbKind::Mongo,
            host: self.host.read(cx).value().trim().to_string(),
            port,
            user: self.user.read(cx).value().trim().to_string(),
            database: self.database.read(cx).value().trim().to_string(),
            options: self.options.read(cx).value().trim().to_string(),
        })
    }

    /// The URL described by the form (without password).
    fn current_url(&self, cx: &App) -> Result<String, String> {
        match self.mode {
            Mode::Fields => Ok(conn_url::build(self.kind, &self.parts(cx)?)),
            Mode::Url => {
                let url = self.url.read(cx).value().trim().to_string();
                Ok(if url.is_empty() { self.kind.default_url().to_string() } else { url })
            }
        }
    }

    fn after_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.name_touched
            && let Ok(url) = self.current_url(cx) {
                let suggestion = suggest_name(&url);
                self.name.update(cx, |s, cx| s.set_value(suggestion, window, cx));
            }
        self.test_result = None;
        cx.notify();
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == self.mode {
            return;
        }
        match mode {
            Mode::Url => match self.current_url(cx) {
                Ok(url) => {
                    self.url.update(cx, |s, cx| s.set_value(url, window, cx));
                    self.mode = Mode::Url;
                }
                Err(e) => self.test_result = Some(Err(e)),
            },
            Mode::Fields => {
                let text = self.url.read(cx).value().trim().to_string();
                if text.is_empty() {
                    self.mode = Mode::Fields;
                } else {
                    match conn_url::parse(&split_url_password(&text).0) {
                        Ok((kind, parts)) => {
                            self.set_kind(kind, window, cx);
                            self.fill_fields(&parts, window, cx);
                            self.mode = Mode::Fields;
                        }
                        Err(e) => {
                            self.test_result = Some(Err(format!("Can't split this URL into fields: {e}")))
                        }
                    }
                }
            }
        }
        let focus = self.initial_focus();
        focus.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    fn config(&self, cx: &App) -> Result<ConnectionConfig, String> {
        let url = self.current_url(cx)?;
        match DbKind::from_url(&url) {
            Some(k) if k != self.kind => {
                return Err(format!("The URL is a {} URL but the type is {}", k.label(), self.kind.label()));
            }
            None => return Err("The URL must start with postgres:// or mongodb://".into()),
            _ => {}
        }
        let mut name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            name = suggest_name(&url);
        }
        let pw = self.password.read(cx).value().to_string();
        Ok(ConnectionConfig {
            id: self.id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name,
            kind: self.kind,
            url,
            password: (!pw.is_empty()).then_some(pw),
            read_only: self.read_only,
            color: self.color.clone(),
            database_colors: self.database_colors.clone(),
            show_all_databases: self.show_all_databases,
        })
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        let config = match self.config(cx) {
            Ok(c) => c,
            Err(e) => {
                self.test_result = Some(Err(e));
                cx.notify();
                return;
            }
        };
        self.testing = true;
        self.test_result = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let started = std::time::Instant::now();
            let result = io(async move {
                let conn = Connection::open(config).await?;
                Ok(conn.server_version())
            })
            .await;
            this.update(cx, |this, cx| {
                this.testing = false;
                this.test_result = Some(match result {
                    Ok(v) => Ok(format!("Connected to {v} in {}", crate::util::format_duration(started.elapsed()))),
                    Err(e) => Err(dbclaw_drivers::error_message(&e)),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn render_fields(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mongo = self.kind == DbKind::Mongo;
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().child(field(
                        if mongo && self.srv { "Host (SRV record)" } else { "Host" },
                        Input::new(&self.host),
                    )))
                    .when(!self.srv, |this| {
                        this.child(div().w(px(96.)).child(field("Port", Input::new(&self.port))))
                    }),
            )
            .when(mongo, |this| {
                this.child(
                    Checkbox::new("srv")
                        .label("DNS seed list (mongodb+srv://, e.g. MongoDB Atlas)")
                        .checked(self.srv)
                        .on_click(cx.listener(|this, checked: &bool, window, cx| {
                            this.srv = *checked;
                            this.after_edit(window, cx);
                        })),
                )
            })
            .child(field("Database", Input::new(&self.database)))
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(field("User", Input::new(&self.user))))
                    .child(div().flex_1().child(field("Password", Input::new(&self.password).mask_toggle()))),
            )
            .child(field("Options", Input::new(&self.options)))
    }

    fn render_url(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(field("Connection URL", Input::new(&self.url)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match self.kind {
                                DbKind::Postgres => "postgres://user@host:5432/database?sslmode=prefer",
                                DbKind::Mongo => "mongodb://user@host:27017/database?authSource=admin  ·  mongodb+srv://…",
                            }),
                    ),
            )
            .child(field("Password", Input::new(&self.password).mask_toggle()))
    }
}

pub fn suggest_name(url: &str) -> String {
    // host[:port]/db → "db @ host"
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(',').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let db = path.split(['?', '#']).next().unwrap_or("");
    let db = dbclaw_core::model::percent_decode(db);
    match (host.is_empty(), db.is_empty()) {
        (true, _) => String::new(),
        (false, true) => host.to_string(),
        (false, false) => format!("{db} @ {host}"),
    }
}

fn field(label: &'static str, input: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(div().text_sm().font_medium().child(label)).child(input)
}

impl ConnectionForm {
    /// Colour tag swatches: none + presets. Shown in the sidebar and on tabs.
    fn render_colors(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let swatch = |id: usize, color: Option<Hsla>, value: Option<&'static str>, label: &'static str| {
            let selected = self.color.as_deref() == value;
            div()
                .id(("color", id))
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(if selected { theme.foreground } else { gpui_kit::transparent_black() })
                .cursor_pointer()
                .child(
                    div()
                        .size(px(16.))
                        .rounded_full()
                        .map(|this| match color {
                            Some(c) => this.bg(c),
                            // "None": an empty ring.
                            None => this.border_1().border_color(theme.muted_foreground),
                        }),
                )
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = value.map(str::to_string);
                    cx.notify();
                }))
        };
        h_flex()
            .gap_1()
            .child(swatch(0, None, None, "None"))
            .children(crate::conn_color::PRESETS.iter().enumerate().map(|(i, (name, label))| {
                swatch(i + 1, crate::conn_color::resolve(Some(name), cx), Some(name), label)
            }))
    }
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let kind = self.kind;
        let mode = self.mode;
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_4()
                    .items_end()
                    .child(field(
                        "Type",
                        ButtonGroup::new("kind")
                            .small()
                            .outline()
                            .child(
                                Button::new("pg")
                                    .icon(Icon::new(Lucide::Database))
                                    .label("PostgreSQL")
                                    .selected(kind == DbKind::Postgres),
                            )
                            .child(
                                Button::new("mongo")
                                    .icon(Icon::new(Lucide::Leaf))
                                    .label("MongoDB")
                                    .selected(kind == DbKind::Mongo),
                            )
                            .on_click(cx.listener(|this, selected: &Vec<usize>, window, cx| {
                                let kind = if selected.first() == Some(&1) { DbKind::Mongo } else { DbKind::Postgres };
                                this.set_kind(kind, window, cx);
                                this.after_edit(window, cx);
                            })),
                    ))
                    .child(div().flex_1())
                    .child(field(
                        "Enter as",
                        ButtonGroup::new("mode")
                            .small()
                            .outline()
                            .child(Button::new("fields").label("Fields").selected(mode == Mode::Fields))
                            .child(Button::new("url").label("URL").selected(mode == Mode::Url))
                            .on_click(cx.listener(|this, selected: &Vec<usize>, window, cx| {
                                let mode = if selected.first() == Some(&1) { Mode::Url } else { Mode::Fields };
                                this.set_mode(mode, window, cx);
                            })),
                    )),
            )
            .child(match mode {
                Mode::Fields => self.render_fields(cx).into_any_element(),
                Mode::Url => self.render_url(cx).into_any_element(),
            })
            .child(div().h(px(1.)).bg(theme.border))
            .child(field("Name", Input::new(&self.name)))
            .child(field("Color", self.render_colors(cx)))
            .child(
                Checkbox::new("show-all-databases")
                    .label("Show all databases (otherwise only the one entered above)")
                    .checked(self.show_all_databases)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.show_all_databases = *checked;
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("read-only")
                    .label("Read-only (disable editing in the data grid)")
                    .checked(self.read_only)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.read_only = *checked;
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("test")
                            .outline()
                            .small()
                            .icon(Icon::new(Lucide::Plug))
                            .label("Test connection")
                            .loading(self.testing)
                            .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                    )
                    .when_some(self.test_result.clone(), |this, r| {
                        let (ok, msg) = match r {
                            Ok(m) => (true, m),
                            Err(m) => (false, m),
                        };
                        this.child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(if ok { theme.success } else { theme.danger })
                                .child(msg),
                        )
                    }),
            )
    }
}

/// Open the dialog; on save the connection is stored and connected.
pub fn open(
    existing: Option<ConnectionConfig>,
    state: Entity<AppState>,
    window: &mut Window,
    cx: &mut App,
) {
    let editing = existing.is_some();
    let form = cx.new(|cx| ConnectionForm::new(existing, window, cx));
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let input = form.read(cx).initial_focus();
            input.focus_handle(cx).focus(window, cx);
        }
        let form_save = form.clone();
        let state = state.clone();
        dialog
            .title(if editing { "Edit connection" } else { "New connection" })
            .w(px(580.))
            .child(form.clone())
            .footer(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel")
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("save")
                            .primary()
                            .label(if editing { "Save" } else { "Save & connect" })
                            .on_click(move |_, window, cx| {
                                let result = form_save.read(cx).config(cx);
                                match result {
                                    Ok(config) => {
                                        let id = config.id.clone();
                                        state.update(cx, |s, cx| {
                                            s.upsert(config, cx);
                                            s.connect(&id, cx);
                                        });
                                        window.close_dialog(cx);
                                    }
                                    Err(e) => form_save.update(cx, |f, cx| {
                                        f.test_result = Some(Err(e));
                                        cx.notify();
                                    }),
                                }
                            }),
                    ),
            )
    });
}

#[cfg(test)]
mod tests {
    use super::suggest_name;

    #[test]
    fn names() {
        assert_eq!(suggest_name("postgres://u@db.example.com:5432/shop?x=1"), "shop @ db.example.com");
        assert_eq!(suggest_name("mongodb://a:1,b:2/?authSource=admin"), "a");
        assert_eq!(suggest_name("mongodb+srv://cluster0.abc.mongodb.net/app"), "app @ cluster0.abc.mongodb.net");
        assert_eq!(suggest_name("postgres://localhost/my%20db"), "my db @ localhost");
    }
}
