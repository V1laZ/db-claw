//! New / edit connection dialog.

use dbclaw_core::model::{ConnectionConfig, DbKind, split_url_password};
use dbclaw_drivers::Connection;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::state::AppState;
use crate::util::io;

pub struct ConnectionForm {
    id: Option<String>,
    kind: DbKind,
    name: Entity<InputState>,
    url: Entity<InputState>,
    password: Entity<InputState>,
    read_only: bool,
    testing: bool,
    test_result: Option<Result<String, String>>,
    name_touched: bool,
    _subs: Vec<Subscription>,
}

impl ConnectionForm {
    fn new(existing: Option<ConnectionConfig>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let kind = existing.as_ref().map_or(DbKind::Postgres, |c| c.kind);
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("My database")
                .default_value(existing.as_ref().map(|c| c.name.clone()).unwrap_or_default())
        });
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(kind.default_url())
                .default_value(existing.as_ref().map(|c| c.url.clone()).unwrap_or_default())
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("optional")
                .default_value(existing.as_ref().and_then(|c| c.password.clone()).unwrap_or_default())
        });
        let subs = vec![
            cx.subscribe_in(&url, window, |this, url, ev: &InputEvent, window, cx| {
                if !matches!(ev, InputEvent::Change) {
                    return;
                }
                let text = url.read(cx).value().to_string();
                if let Some(kind) = DbKind::from_url(&text) {
                    this.kind = kind;
                }
                // Move a pasted password into the password field.
                let (without, pw) = split_url_password(&text);
                if let Some(pw) = pw {
                    url.update(cx, |s, cx| s.set_value(without.clone(), window, cx));
                    this.password.update(cx, |s, cx| s.set_value(pw, window, cx));
                }
                if !this.name_touched {
                    let suggestion = suggest_name(&without);
                    this.name.update(cx, |s, cx| s.set_value(suggestion, window, cx));
                }
                this.test_result = None;
                cx.notify();
            }),
            cx.subscribe(&name, |this, _, ev: &InputEvent, _| {
                if matches!(ev, InputEvent::Change) {
                    this.name_touched = true;
                }
            }),
        ];
        Self {
            name_touched: existing.is_some(),
            id: existing.as_ref().map(|c| c.id.clone()),
            kind,
            name,
            url,
            password,
            read_only: existing.as_ref().is_some_and(|c| c.read_only),
            testing: false,
            test_result: None,
            _subs: subs,
        }
    }

    fn config(&self, cx: &App) -> Result<ConnectionConfig, String> {
        let url = self.url.read(cx).value().trim().to_string();
        let url = if url.is_empty() { self.kind.default_url().to_string() } else { url };
        if let Some(k) = DbKind::from_url(&url) {
            if k != self.kind {
                return Err(format!("The URL is a {} URL but the type is {}", k.label(), self.kind.label()));
            }
        } else {
            return Err("The URL must start with postgres:// or mongodb://".into());
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
}

pub fn suggest_name(url: &str) -> String {
    // host[:port]/db → "db @ host"
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.split(',').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let db = path.split(['?', '#']).next().unwrap_or("");
    match (host.is_empty(), db.is_empty()) {
        (true, _) => String::new(),
        (false, true) => host.to_string(),
        (false, false) => format!("{db} @ {host}"),
    }
}

fn field(label: &'static str, input: impl IntoElement) -> impl IntoElement {
    v_flex().gap_1().child(div().text_sm().font_medium().child(label)).child(input)
}

impl Render for ConnectionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let kind = self.kind;
        v_flex()
            .gap_3()
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
                        if kind != this.kind {
                            this.kind = kind;
                            this.url.update(cx, |s, cx| {
                                s.set_placeholder(kind.default_url(), window, cx);
                            });
                            this.test_result = None;
                            cx.notify();
                        }
                    })),
            ))
            .child(field("Connection URL", Input::new(&self.url)))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(match kind {
                        DbKind::Postgres => "postgres://user@host:5432/database?sslmode=prefer",
                        DbKind::Mongo => "mongodb://user@host:27017/database?authSource=admin  ·  mongodb+srv://…",
                    }),
            )
            .child(field("Password", Input::new(&self.password).mask_toggle()))
            .child(field("Name", Input::new(&self.name)))
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
            let url = form.read(cx).url.clone();
            url.focus_handle(cx).focus(window, cx);
        }
        let form_save = form.clone();
        let state = state.clone();
        dialog
            .title(if editing { "Edit connection" } else { "New connection" })
            .w(px(560.))
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
    }
}
