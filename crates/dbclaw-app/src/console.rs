//! Native query console: SQL for Postgres, mongosh syntax for Mongo.

use std::time::Duration;

use dbclaw_core::model::{DbKind, QueryOutput, ResultSet};
use dbclaw_core::sql;
use dbclaw_drivers::{Connection, PgExecError, Session};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex,
    v_resizable, resizable_panel,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use std::rc::Rc;

use gpui_kit::component::input::CompletionProvider;

use crate::actions::*;
use crate::completion::{MongoCompletion, SharedCatalog, SqlCompletion};
use crate::grid::GridDelegate;
use crate::util::{format_duration, group_digits};

const ROW_LIMIT: usize = 10_000;

pub struct ConsoleView {
    pub conn_id: String,
    pub conn_name: SharedString,
    conn: Connection,
    pub database: String,
    pub collection: Option<String>,
    editor: Entity<EditorState>,
    session: Option<Session>,
    running: Option<tokio::task::AbortHandle>,
    run_id: u64,
    output: Option<QueryOutput>,
    error: Option<SharedString>,
    selected_set: usize,
    last_elapsed: Option<Duration>,
    ran_text: Option<SharedString>,
    table: Entity<TableState<GridDelegate>>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl ConsoleView {
    pub fn new(
        conn_id: String,
        conn_name: SharedString,
        conn: Connection,
        database: String,
        collection: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (language, initial) = match conn.kind() {
            DbKind::Postgres => ("sql", "-- ⌘↵ runs the statement at the cursor (or the selection), ⇧⌘↵ runs everything\nSELECT now();\n".to_string()),
            DbKind::Mongo => {
                let c = collection.clone().unwrap_or_else(|| "collection".into());
                let ident_ok = c.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
                let target = if ident_ok { format!("db.{c}") } else { format!("db.getCollection({c:?})") };
                (
                    "javascript",
                    format!(
                        "// ⌘↵ runs the block at the cursor (or the selection). Blocks are separated by blank lines.\n{target}.find({{}}).limit(50)\n"
                    ),
                )
            }
        };
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .default_value(initial)
        });
        // Autocompletion from the database's tables/collections (loaded in
        // the background; suggestions appear as soon as it's there).
        let catalog = SharedCatalog::load(conn.clone(), database.clone(), cx);
        let provider: Rc<dyn CompletionProvider> = match conn.kind() {
            DbKind::Postgres => Rc::new(SqlCompletion { catalog }),
            DbKind::Mongo => Rc::new(MongoCompletion {
                conn: conn.clone(),
                database: database.clone(),
                catalog,
                default_collection: collection.clone(),
                fields: Default::default(),
            }),
        };
        editor.update(cx, |e, _| e.lsp_mut().completion_provider = Some(provider));
        let table = cx.new(|cx| {
            TableState::new(GridDelegate::new(), window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .row_header(false)
                .col_movable(false)
                .sortable(true)
        });
        let subs = vec![cx.subscribe_in(&editor, window, |this, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { secondary: true, shift } = ev {
                this.run(*shift, window, cx);
            }
        })];
        Self {
            conn_id,
            conn_name,
            conn,
            database,
            collection,
            editor,
            session: None,
            running: None,
            run_id: 0,
            output: None,
            error: None,
            selected_set: 0,
            last_elapsed: None,
            ran_text: None,
            table,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn title(&self) -> SharedString {
        let kind = match self.conn.kind() {
            DbKind::Postgres => "SQL",
            DbKind::Mongo => "Mongo",
        };
        match &self.collection {
            Some(c) => format!("{kind} · {}.{c}", self.database).into(),
            None => format!("{kind} · {}", self.database).into(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn editor_focus(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }

    /// The text to run: the selection, else the statement (SQL) or blank-line
    /// separated block (Mongo) at the cursor, or everything when `all`.
    fn text_to_run(&self, all: bool, cx: &App) -> String {
        let state = self.editor.read(cx);
        let text = state.value().to_string();
        if all {
            return text;
        }
        let sel = state.selected_range();
        if sel.start < sel.end && sel.end <= text.len() {
            return text[sel].to_string();
        }
        let cursor = state.cursor().min(text.len());
        match self.conn.kind() {
            DbKind::Postgres => sql::statement_at(&text, cursor)
                .map(|r| text[r].to_string())
                .unwrap_or(text),
            DbKind::Mongo => block_at(&text, cursor).to_string(),
        }
    }

    fn run(&mut self, all: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let text = self.text_to_run(all, cx);
        if text.trim().is_empty() {
            return;
        }
        self.run_id += 1;
        let run_id = self.run_id;
        self.error = None;
        self.ran_text = Some(crate::util::single_line(text.trim(), 120).into());
        let conn = self.conn.clone();
        let database = self.database.clone();
        let existing = self.session.clone().filter(|s| !s.is_closed());
        let collection = self.collection.clone();
        let handle = dbclaw_drivers::spawn(async move {
            let session = match existing {
                Some(s) => s,
                None => conn.open_session(&database).await?,
            };
            let out = session.execute(&text, ROW_LIMIT, collection.as_deref()).await;
            Ok::<_, anyhow::Error>((session, out))
        });
        self.running = Some(handle.abort_handle());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            this.update_in(cx, |this, _, cx| {
                if this.run_id != run_id {
                    return;
                }
                this.running = None;
                match result {
                    Err(e) if e.is_cancelled() => {
                        this.error = Some("Cancelled".into());
                    }
                    Err(e) => this.error = Some(format!("{e}").into()),
                    Ok(Err(e)) => this.error = Some(dbclaw_drivers::error_message(&e).into()),
                    Ok(Ok((session, out))) => {
                        this.session = Some(session);
                        match out {
                            Ok(output) => this.show_output(output, None, cx),
                            Err(e) => {
                                let msg: SharedString = dbclaw_drivers::error_message(&e).into();
                                match e.downcast::<PgExecError>() {
                                    Ok(pe) if !pe.partial.sets.is_empty() => {
                                        this.show_output(pe.partial, Some(msg), cx)
                                    }
                                    _ => this.error = Some(msg),
                                }
                            }
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_output(&mut self, output: QueryOutput, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.error = error;
        self.last_elapsed = Some(output.elapsed);
        // Prefer the last result that has rows.
        self.selected_set = output
            .sets
            .iter()
            .rposition(|s| !s.columns.is_empty())
            .unwrap_or(output.sets.len().saturating_sub(1));
        self.output = Some(output);
        self.show_set(self.selected_set, cx);
    }

    fn show_set(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected_set = ix;
        let set = self
            .output
            .as_ref()
            .and_then(|o| o.sets.get(ix).cloned())
            .unwrap_or_default();
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            d.sort = None;
            d.set_data(set, 0);
            t.refresh(cx);
        });
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(abort) = self.running.take() else { return };
        match &self.session {
            Some(Session::Pg(_)) => {
                // Ask the server to stop; the run finishes with an error.
                let session = self.session.clone().unwrap();
                self.running = Some(abort);
                dbclaw_drivers::spawn(async move { session.cancel().await });
            }
            _ => {
                abort.abort();
                self.run_id += 1;
                self.error = Some("Cancelled".into());
            }
        }
        cx.notify();
    }

    fn on_run(&mut self, _: &RunQuery, window: &mut Window, cx: &mut Context<Self>) {
        self.run(false, window, cx);
    }

    fn on_run_all(&mut self, _: &RunAll, window: &mut Window, cx: &mut Context<Self>) {
        self.run(true, window, cx);
    }

    fn on_cancel(&mut self, _: &CancelQuery, _: &mut Window, cx: &mut Context<Self>) {
        self.cancel(cx);
    }

    fn render_results(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let sets: Vec<(usize, SharedString)> = self
            .output
            .as_ref()
            .map(|o| {
                o.sets
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (i, set_label(i, s)))
                    .collect()
            })
            .unwrap_or_default();
        let current = self.output.as_ref().and_then(|o| o.sets.get(self.selected_set));
        let notices: Vec<String> = self.output.as_ref().map(|o| o.notices.clone()).unwrap_or_default();

        let status: SharedString = if self.running.is_some() {
            "Running…".into()
        } else if let Some(set) = current {
            let mut s = set.status.clone().unwrap_or_default();
            if let Some(e) = self.last_elapsed {
                s = format!("{s} · {}", format_duration(e));
            }
            s.into()
        } else {
            "".into()
        };

        let header = h_flex()
            .gap_1()
            .px_2()
            .h(px(32.))
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .when(sets.len() > 1, |this| {
                this.children(sets.iter().map(|(i, label)| {
                    let i = *i;
                    Button::new(("set", i))
                        .xsmall()
                        .ghost()
                        .label(label.clone())
                        .selected(i == self.selected_set)
                        .on_click(cx.listener(move |this, _, _, cx| this.show_set(i, cx)))
                }))
            })
            .child(div().flex_1())
            .child(div().text_color(theme.muted_foreground).child(status));

        let has_grid = current.is_some_and(|s| !s.columns.is_empty());
        let body = if self.output.is_none() && self.error.is_none() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.running.is_some() { "Running…" } else { "Run a query with ⌘↵" })
                .into_any_element()
        } else if has_grid {
            div().flex_1().min_h_0().child(DataTable::new(&self.table).stripe(true).bordered(false)).into_any_element()
        } else {
            v_flex()
                .flex_1()
                .p_3()
                .text_sm()
                .text_color(theme.muted_foreground)
                .when_some(current.and_then(|s| s.status.clone()), |this, s| this.child(s))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(header)
            .when_some(self.error.clone(), |this, e| {
                this.child(
                    h_flex()
                        .items_start()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .bg(theme.danger.opacity(0.08))
                        .text_color(theme.danger)
                        .child(Icon::new(Lucide::CircleAlert).small())
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .font_family(theme.mono_font_family.clone())
                                .whitespace_normal()
                                .child(e),
                        ),
                )
            })
            .when(!notices.is_empty(), |this| {
                this.child(
                    v_flex()
                        .px_3()
                        .py_1()
                        .gap_0p5()
                        .bg(theme.info.opacity(0.08))
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .children(notices.into_iter().map(|n| div().child(n))),
                )
            })
            .child(body)
            .into_any_element()
    }
}

fn set_label(i: usize, s: &ResultSet) -> SharedString {
    if s.columns.is_empty() {
        format!("{}: {}", i + 1, s.status.clone().unwrap_or_else(|| "ok".into())).into()
    } else {
        format!("{}: {} rows", i + 1, group_digits(s.rows.len() as u64)).into()
    }
}

/// The blank-line separated block containing `cursor`.
fn block_at(text: &str, cursor: usize) -> &str {
    let mut start = 0;
    let mut blocks = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            if offset > start {
                blocks.push(start..offset);
            }
            start = offset + line.len();
        }
        offset += line.len();
    }
    if text.len() > start {
        blocks.push(start..text.len());
    }
    let pick = blocks
        .iter()
        .find(|r| cursor >= r.start && cursor <= r.end)
        .or_else(|| blocks.iter().rev().find(|r| r.end <= cursor))
        .or(blocks.first())
        .cloned();
    // Drop comment-only lines so "// hint" + command works.
    match pick {
        Some(r) => text[r].trim(),
        None => "",
    }
}

impl Focusable for ConsoleView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConsoleView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let running = self.running.is_some();
        let toolbar = h_flex()
            .gap_1()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("run")
                    .primary()
                    .small()
                    .icon(Icon::new(Lucide::Play))
                    .label("Run")
                    .tooltip("Run statement at cursor / selection (⌘↵)")
                    .loading(running)
                    .on_click(cx.listener(|this, _, window, cx| this.run(false, window, cx))),
            )
            .child(
                Button::new("run-all")
                    .ghost()
                    .small()
                    .label("Run all")
                    .tooltip("Run everything (⇧⌘↵)")
                    .on_click(cx.listener(|this, _, window, cx| this.run(true, window, cx))),
            )
            .when(running, |this| {
                this.child(
                    Button::new("cancel")
                        .danger()
                        .small()
                        .icon(Icon::new(Lucide::Square))
                        .label("Cancel")
                        .tooltip("Cancel (⌘.)")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(match self.conn.kind() {
                        DbKind::Postgres => Lucide::Database,
                        DbKind::Mongo => Lucide::Leaf,
                    }).xsmall())
                    .child(format!("{} / {}", self.conn_name, self.database)),
            );

        v_flex()
            .id("console")
            .key_context("Console")
            .track_focus(&self.focus)
            .size_full()
            .on_action(cx.listener(Self::on_run))
            .on_action(cx.listener(Self::on_run_all))
            .on_action(cx.listener(Self::on_cancel))
            .child(toolbar)
            .child(
                v_resizable("console-split")
                    .child(
                        resizable_panel()
                            .size(px(260.))
                            .child(div().size_full().child(Editor::new(&self.editor).h_full().bordered(false))),
                    )
                    .child(resizable_panel().child(self.render_results(cx))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::block_at;

    #[test]
    fn blocks() {
        let t = "db.a.find()\n\ndb.b.find({\n  x: 1\n})\n\n\ndb.c.count()";
        assert_eq!(block_at(t, 0), "db.a.find()");
        assert_eq!(block_at(t, 15), "db.b.find({\n  x: 1\n})");
        assert_eq!(block_at(t, t.len()), "db.c.count()");
        assert_eq!(block_at("", 0), "");
    }
}

// ---- automation API ----------------------------------------------------------

impl ConsoleView {
    pub fn run_id(&self) -> u64 {
        self.run_id
    }

    /// Replace the editor text (when given) and run. Returns the run id to
    /// wait for.
    pub fn start_run(&mut self, text: Option<&str>, all: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<u64, String> {
        if self.running.is_some() {
            return Err("a query is already running in this console".into());
        }
        if let Some(t) = text {
            self.editor.update(cx, |e, cx| e.set_value(t.to_string(), window, cx));
        }
        let before = self.run_id;
        self.run(all || text.is_some(), window, cx);
        if self.run_id == before {
            return Err("nothing to run".into());
        }
        Ok(self.run_id)
    }

    pub fn snapshot(&self, rows_per_set: usize, cx: &App) -> serde_json::Value {
        use serde_json::json;
        let sets: Vec<serde_json::Value> = self
            .output
            .as_ref()
            .map(|o| {
                o.sets
                    .iter()
                    .map(|s| {
                        let mut d = crate::grid::GridDelegate::new();
                        d.set_data(s.clone(), 0);
                        let mut v = crate::table_view::grid_json(&d, 0, rows_per_set);
                        v["status"] = json!(s.status);
                        v["row_count"] = json!(s.rows.len());
                        v["affected"] = json!(s.affected);
                        v
                    })
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "kind": "console",
            "connection": self.conn_name.to_string(),
            "connection_id": self.conn_id,
            "database": self.database,
            "collection": self.collection,
            "text": self.editor.read(cx).value().to_string(),
            "running": self.running.is_some(),
            "error": self.error.as_ref().map(|e| e.to_string()),
            "notices": self.output.as_ref().map(|o| o.notices.clone()).unwrap_or_default(),
            "elapsed_ms": self.last_elapsed.map(|e| e.as_secs_f64() * 1000.0),
            "result_sets": sets,
        })
    }
}
