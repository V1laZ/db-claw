//! Data tab for one table / view / collection: filter bar, server-side
//! sorting and paging, inline editing with staged changes.

use std::rc::Rc;
use std::time::{Duration, Instant};

use dbclaw_core::filter::{self, Expr};
use dbclaw_core::model::{
    Cell, ColumnMeta, DbKind, NewValue, ObjectRef, PageRequest, RowChange, RowKey, SortDir,
    SortSpec,
};
use dbclaw_core::mongo_shell;
use dbclaw_drivers::Connection;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::table::{ColumnSort, DataTable, TableEvent, TableSelection, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::*;
use crate::grid::{CellEditor, GridDelegate, PendingCell};
use crate::record_dialog;
use crate::util::{format_duration, group_digits, io};

/// Notification key for save errors (one per tab, cleared on success).
struct SaveError;

pub type OnSuccess = Box<dyn FnOnce(&mut Window, &mut App)>;
type RowAction = Rc<dyn Fn(&mut TableView, &mut Window, &mut Context<TableView>)>;

pub const PAGE_SIZES: [u64; 4] = [100, 500, 1000, 5000];

pub struct TableView {
    pub conn_id: String,
    pub conn_name: SharedString,
    conn: Connection,
    pub object: ObjectRef,
    editable: bool,
    columns: Vec<ColumnMeta>,
    columns_loaded: bool,
    table: Entity<TableState<GridDelegate>>,
    filter_input: Entity<InputState>,
    filter_error: Option<SharedString>,
    filter: Option<Expr>,
    sort: Vec<SortSpec>,
    page: u64,
    page_size: u64,
    total: Option<u64>,
    counting: bool,
    loading: bool,
    saving: bool,
    save_seq: u64,
    pending_placeholder: Option<SharedString>,
    last_save: Option<(u64, Result<String, String>)>,
    error: Option<SharedString>,
    elapsed: Option<Duration>,
    request: u64,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl TableView {
    pub fn new(
        conn_id: String,
        conn_name: SharedString,
        conn: Connection,
        object: ObjectRef,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editable = !read_only && object.kind.is_editable();
        let weak = cx.entity().downgrade();
        let mut delegate = GridDelegate::new();
        delegate.loading = true;
        {
            let weak = weak.clone();
            delegate.on_sort = Some(Rc::new(move |sort, window, cx| {
                weak.update(cx, |this, cx| this.set_sort(sort, window, cx)).ok();
            }));
        }
        {
            let weak = weak.clone();
            delegate.context_menu = Some(Rc::new(move |row, menu, _window, cx| {
                let Some(view) = weak.upgrade() else { return menu };
                view.read(cx).row_menu(row, menu, weak.clone(), cx)
            }));
        }
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .cell_selectable(true)
                .row_selectable(true)
                .row_header(false)
                .col_movable(false)
                .sortable(true)
        });
        let placeholder = match conn.kind() {
            DbKind::Postgres => "Filter: status = \"paid\" and total > 100 and note is not null",
            DbKind::Mongo => "Filter: age >= 30 and address.city = \"Prague\" and tags in [\"beta\"]",
        };
        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subs = vec![
            cx.subscribe_in(&table, window, Self::on_table_event),
            cx.subscribe_in(&filter_input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.apply_filter(window, cx),
                InputEvent::Change
                    if this.filter_error.is_some() => {
                        this.filter_error = None;
                        cx.notify();
                    }
                _ => {}
            }),
        ];
        let mut this = Self {
            conn_id,
            conn_name,
            conn,
            object,
            editable,
            columns: Vec::new(),
            columns_loaded: false,
            table,
            filter_input,
            filter_error: None,
            filter: None,
            sort: Vec::new(),
            page: 0,
            page_size: 500,
            total: None,
            counting: false,
            loading: true,
            saving: false,
            save_seq: 0,
            pending_placeholder: None,
            last_save: None,
            error: None,
            elapsed: None,
            request: 0,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.reload(true, cx);
        this
    }

    pub fn title(&self) -> SharedString {
        self.object.display_name().into()
    }

    pub fn has_changes(&self, cx: &App) -> bool {
        self.table.read(cx).delegate().has_changes()
    }

    pub fn kind(&self) -> DbKind {
        self.conn.kind()
    }

    /// Focus target when the tab is activated: the grid itself, so arrow
    /// keys and Enter work right away.
    pub fn grid_focus(&self, cx: &App) -> FocusHandle {
        self.table.focus_handle(cx)
    }

    fn page_request(&self) -> PageRequest {
        PageRequest {
            object: self.object.clone(),
            filter: self.filter.clone(),
            sort: self.sort.clone(),
            offset: self.page * self.page_size,
            limit: self.page_size,
        }
    }

    /// Fetch the current page (and the row count when `recount`).
    pub fn reload(&mut self, recount: bool, cx: &mut Context<Self>) {
        self.request += 1;
        let request = self.request;
        self.loading = true;
        self.error = None;
        self.table.update(cx, |t, cx| {
            t.delegate_mut().loading = true;
            cx.notify();
        });
        cx.notify();

        let conn = self.conn.clone();
        let req = self.page_request();
        let need_columns = !self.columns_loaded;
        let columns = self.columns.clone();
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let result = io(async move {
                let columns = if need_columns { conn.columns(&req.object).await? } else { columns };
                let page = conn.fetch_page(&req, &columns).await?;
                Ok((columns, page))
            })
            .await;
            this.update(cx, |this, cx| {
                if this.request != request {
                    return; // superseded
                }
                this.loading = false;
                this.elapsed = Some(started.elapsed());
                match result {
                    Ok((columns, page)) => {
                        if need_columns {
                            this.columns = columns;
                            this.columns_loaded = true;
                            let source = if this.columns.is_empty() { &page.columns } else { &this.columns };
                            this.pending_placeholder = Some(filter_placeholder(source));
                        }
                        let offset = (this.page * this.page_size) as usize;
                        let sort = this.sort.first().and_then(|s| {
                            page.columns.iter().position(|c| c.name == s.column).map(|i| {
                                (i, if s.dir == SortDir::Asc { ColumnSort::Ascending } else { ColumnSort::Descending })
                            })
                        });
                        let rows = page.rows.len();
                        this.table.update(cx, |t, cx| {
                            let d = t.delegate_mut();
                            d.loading = false;
                            d.sort = sort;
                            d.set_data(page, offset);
                            t.refresh(cx);
                            if rows > 0 {
                                t.scroll_to_row(0, cx);
                            }
                        });
                        // An unfiltered single page: the count is exact already.
                        if this.page == 0 && (rows as u64) < this.page_size {
                            this.total = Some(rows as u64);
                            this.counting = false;
                        }
                    }
                    Err(e) => {
                        this.error = Some(dbclaw_drivers::error_message(&e).into());
                        this.table.update(cx, |t, cx| {
                            t.delegate_mut().loading = false;
                            cx.notify();
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        if recount {
            self.count(cx);
        }
    }

    fn count(&mut self, cx: &mut Context<Self>) {
        self.total = None;
        self.counting = true;
        let request = self.request;
        let conn = self.conn.clone();
        let req = self.page_request();
        let columns = self.columns.clone();
        let need_columns = !self.columns_loaded;
        cx.spawn(async move |this, cx| {
            let result = io(async move {
                let columns = if need_columns { conn.columns(&req.object).await? } else { columns };
                conn.count(&req, &columns).await
            })
            .await;
            this.update(cx, |this, cx| {
                if this.request < request {
                    return;
                }
                this.counting = false;
                if let Ok(n) = result {
                    this.total = Some(n);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_sort(&mut self, sort: Option<(String, ColumnSort)>, _: &mut Window, cx: &mut Context<Self>) {
        self.sort = match sort {
            Some((column, ColumnSort::Ascending)) => vec![SortSpec { column, dir: SortDir::Asc }],
            Some((column, ColumnSort::Descending)) => vec![SortSpec { column, dir: SortDir::Desc }],
            _ => vec![],
        };
        self.page = 0;
        self.reload(false, cx);
    }

    fn apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.filter_input.read(cx).value().to_string();
        match filter::parse(&text) {
            Ok(expr) => {
                // Validate column names / compile before hitting the server.
                if let Some(e) = &expr {
                    let check = match self.conn.kind() {
                        DbKind::Postgres => {
                            let cols: Vec<filter::SqlColumn> = self
                                .columns
                                .iter()
                                .map(|c| filter::SqlColumn { name: c.name.clone(), data_type: c.type_name.clone() })
                                .collect();
                            filter::to_sql(e, &cols).map(|_| ())
                        }
                        DbKind::Mongo => filter::to_mongo(e).map(|_| ()),
                    };
                    if let Err(err) = check {
                        self.show_filter_error(&text, err.message, err.span, window, cx);
                        return;
                    }
                }
                self.filter_error = None;
                self.filter = expr;
                self.page = 0;
                self.reload(true, cx);
            }
            Err(err) => self.show_filter_error(&text, err.message, err.span, window, cx),
        }
    }

    fn show_filter_error(
        &mut self,
        text: &str,
        message: String,
        span: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let near = text.get(span.clone()).filter(|s| !s.is_empty());
        self.filter_error = Some(match near {
            Some(s) => format!("{message} (at “{}”)", crate::util::single_line(s, 30)).into(),
            None => message.into(),
        });
        let start = span.start.min(text.len());
        let end = span.end.clamp(start, text.len());
        self.filter_input.update(cx, |s, cx| s.set_selected_range(start..end, cx));
        cx.notify();
    }

    /// Add `column = value` (or `is null`) to the filter and apply it.
    fn filter_by_cell(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        let d = self.table.read(cx).delegate();
        let Some(meta) = d.column_meta(col) else { return };
        let field = if meta.name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            meta.name.clone()
        } else {
            format!("`{}`", meta.name.replace('`', ""))
        };
        let cond = match (self.conn.kind(), d.set.documents.as_ref()) {
            (DbKind::Mongo, Some(docs)) => match docs.get(row).and_then(|doc| doc.get(&meta.name)) {
                None => format!("{field} not exists"),
                Some(bson::Bson::Null) => format!("{field} is null"),
                Some(bson::Bson::ObjectId(oid)) => format!("{field} = oid(\"{oid}\")"),
                Some(bson::Bson::Int32(n)) => format!("{field} = {n}"),
                Some(bson::Bson::Int64(n)) => format!("{field} = {n}"),
                Some(bson::Bson::Double(n)) => format!("{field} = {n:?}"),
                Some(bson::Bson::Boolean(b)) => format!("{field} = {b}"),
                Some(bson::Bson::String(s)) => format!("{field} = {}", quote_str(s)),
                Some(bson::Bson::DateTime(dt)) => format!(
                    "{field} = date(\"{}\")",
                    dt.try_to_rfc3339_string().unwrap_or_default()
                ),
                Some(other) => format!("{field} = {}", quote_str(&mongo_shell::format_cell(other))),
            },
            _ => match d.cell(row, col) {
                Some(Cell::Value(v, _)) => format!("{field} = {}", quote_str(v)),
                _ => format!("{field} is null"),
            },
        };
        let current = self.filter_input.read(cx).value().trim().to_string();
        let new = if current.is_empty() { cond } else { format!("{current} and {cond}") };
        self.filter_input.update(cx, |s, cx| s.set_value(new, window, cx));
        self.apply_filter(window, cx);
    }

    fn on_table_event(
        &mut self,
        _: &Entity<TableState<GridDelegate>>,
        ev: &TableEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let TableEvent::DoubleClickedCell(row, col) = ev
            && *col > 0 {
                self.start_edit(*row, col - 1, window, cx);
            }
    }

    fn selected_row(&self, cx: &App) -> Option<usize> {
        match self.table.read(cx).selection() {
            TableSelection::Row(r) | TableSelection::Cell(r, _) => Some(r),
            _ => None,
        }
    }

    fn selected_cell(&self, cx: &App) -> Option<(usize, usize)> {
        match self.table.read(cx).selection() {
            TableSelection::Cell(r, c) if c > 0 => Some((r, c - 1)),
            _ => None,
        }
    }

    fn can_edit_cell(&self, row: usize, col: usize, cx: &App) -> Result<(), &'static str> {
        if !self.editable {
            return Err(if self.object.kind.is_editable() {
                "This connection is read-only"
            } else {
                "Views can't be edited"
            });
        }
        let d = self.table.read(cx).delegate();
        if d.deleted.contains(&row) {
            return Err("Row is marked for deletion");
        }
        let meta = d.column_meta(col).ok_or("No such column")?;
        if meta.generated {
            return Err("Generated columns can't be edited");
        }
        if self.conn.kind() == DbKind::Mongo && meta.name == "_id" {
            return Err("_id can't be changed");
        }
        if self.conn.kind() == DbKind::Postgres
            && !d.set.columns.iter().any(|c| c.is_pk)
            && d.set.ctids.is_none()
        {
            return Err("Rows without a primary key can't be edited");
        }
        Ok(())
    }

    fn start_edit(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(msg) = self.can_edit_cell(row, col, cx) {
            window.push_notification(Notification::warning(msg), cx);
            return;
        }
        // Nested documents are much nicer to edit as a whole document.
        if self.conn.kind() == DbKind::Mongo {
            let d = self.table.read(cx).delegate();
            let name = d.set.columns[col].name.clone();
            let nested = d
                .set
                .documents
                .as_ref()
                .and_then(|docs| docs.get(row))
                .and_then(|doc| doc.get(&name))
                .is_some_and(|v| matches!(v, bson::Bson::Document(_) | bson::Bson::Array(_)));
            if nested {
                self.edit_document(row, window, cx);
                return;
            }
        }
        let text = self.table.read(cx).delegate().cell_value_text(row, col).unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
        let sub = cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.commit_edit(true, window, cx),
            InputEvent::Blur => this.commit_edit(false, window, cx),
            _ => {}
        });
        self._subs.push(sub);
        input.update(cx, |s, cx| s.focus(window, cx));
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editor = Some(CellEditor { row, col, input });
            cx.notify();
        });
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let had = self.table.update(cx, |t, cx| {
            cx.notify();
            t.delegate_mut().editor.take().is_some()
        });
        if had {
            self.table.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    fn commit_edit(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.table.update(cx, |t, _| t.delegate_mut().editor.take()) else {
            return;
        };
        self._subs.truncate(2);
        let text = editor.input.read(cx).value().to_string();
        let (row, col) = (editor.row, editor.col);
        let result = self.pending_for(row, col, &text, cx);
        match result {
            Ok(Some(p)) => self.table.update(cx, |t, _| {
                t.delegate_mut().edits.insert((row, col), p);
            }),
            Ok(None) => self.table.update(cx, |t, _| {
                t.delegate_mut().edits.remove(&(row, col));
            }),
            Err(e) => window.push_notification(Notification::error(e), cx),
        }
        self.table.update(cx, |_, cx| cx.notify());
        if refocus {
            self.table.read(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    /// Turn edited text into a staged value. `Ok(None)` = unchanged.
    fn pending_for(&self, row: usize, col: usize, text: &str, cx: &App) -> Result<Option<PendingCell>, String> {
        let d = self.table.read(cx).delegate();
        let original = d.set.rows.get(row).and_then(|r| r.get(col)).cloned().unwrap_or(Cell::Missing);
        let meta = &d.set.columns[col];
        match self.conn.kind() {
            DbKind::Postgres => {
                if original.as_str() == Some(text) {
                    return Ok(None);
                }
                Ok(Some(PendingCell {
                    display: Cell::Value(text.into(), meta.kind),
                    value: NewValue::Text(Some(text.to_string())),
                }))
            }
            DbKind::Mongo => {
                let orig = d
                    .set
                    .documents
                    .as_ref()
                    .and_then(|docs| docs.get(row))
                    .and_then(|doc| doc.get(&meta.name))
                    .cloned();
                let unchanged_text = match &orig {
                    Some(v) => mongo_shell::format_cell(v) == text,
                    None => text.is_empty(),
                };
                if unchanged_text {
                    return Ok(None);
                }
                let value = mongo_shell::parse_cell_edit(text, orig.as_ref().unwrap_or(&bson::Bson::Null))
                    .map_err(|e| e.message)?;
                let display = match &value {
                    bson::Bson::Null => Cell::Null,
                    v => Cell::Value(mongo_shell::format_cell(v).into(), dbclaw_drivers::bson_kind(v)),
                };
                Ok(Some(PendingCell { display, value: NewValue::Bson(value) }))
            }
        }
    }

    fn set_null(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(msg) = self.can_edit_cell(row, col, cx) {
            window.push_notification(Notification::warning(msg), cx);
            return;
        }
        let value = match self.conn.kind() {
            DbKind::Postgres => NewValue::Text(None),
            DbKind::Mongo => NewValue::Bson(bson::Bson::Null),
        };
        self.table.update(cx, |t, cx| {
            t.delegate_mut().edits.insert((row, col), PendingCell { display: Cell::Null, value });
            cx.notify();
        });
        cx.notify();
    }

    fn unset_field(&mut self, row: usize, col: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(msg) = self.can_edit_cell(row, col, cx) {
            window.push_notification(Notification::warning(msg), cx);
            return;
        }
        self.table.update(cx, |t, cx| {
            t.delegate_mut()
                .edits
                .insert((row, col), PendingCell { display: Cell::Missing, value: NewValue::Unset });
            cx.notify();
        });
        cx.notify();
    }

    fn toggle_delete(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable {
            window.push_notification(Notification::warning("This object is read-only"), cx);
            return;
        }
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if !d.deleted.remove(&row) {
                d.deleted.insert(row);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn row_key(&self, row: usize, cx: &App) -> Result<RowKey, String> {
        let d = self.table.read(cx).delegate();
        match self.conn.kind() {
            DbKind::Mongo => d
                .set
                .documents
                .as_ref()
                .and_then(|docs| docs.get(row))
                .and_then(|doc| doc.get("_id"))
                .map(|id| RowKey::Id(id.clone()))
                .ok_or_else(|| "document has no _id".to_string()),
            DbKind::Postgres => {
                let pk: Vec<(String, Option<String>)> = d
                    .set
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| c.is_pk)
                    .map(|(i, c)| {
                        let v = d.set.rows[row].get(i).and_then(|c| c.as_str()).map(str::to_string);
                        (c.name.clone(), v)
                    })
                    .collect();
                if !pk.is_empty() {
                    return Ok(RowKey::Pk(pk));
                }
                d.set
                    .ctids
                    .as_ref()
                    .and_then(|c| c.get(row))
                    .map(|c| RowKey::Ctid(c.clone()))
                    .ok_or_else(|| "row has no primary key".to_string())
            }
        }
    }

    fn collect_changes(&self, cx: &App) -> Result<Vec<RowChange>, String> {
        let d = self.table.read(cx).delegate();
        let mut changes = Vec::new();
        let mut rows: Vec<usize> = d.edits.keys().map(|(r, _)| *r).collect();
        rows.sort_unstable();
        rows.dedup();
        for row in rows {
            if d.deleted.contains(&row) {
                continue;
            }
            let mut values: Vec<(usize, String, NewValue)> = d
                .edits
                .iter()
                .filter(|((r, _), _)| *r == row)
                .map(|((_, c), p)| (*c, d.set.columns[*c].name.clone(), p.value.clone()))
                .collect();
            values.sort_by_key(|(c, _, _)| *c);
            changes.push(RowChange::Update {
                key: self.row_key(row, cx)?,
                values: values.into_iter().map(|(_, n, v)| (n, v)).collect(),
            });
        }
        for &row in &d.deleted {
            changes.push(RowChange::Delete { key: self.row_key(row, cx)? });
        }
        Ok(changes)
    }

    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(false, window, cx);
        if self.saving || !self.has_changes(cx) {
            return;
        }
        let changes = match self.collect_changes(cx) {
            Ok(c) => c,
            Err(e) => {
                window.push_notification(Notification::error(e), cx);
                return;
            }
        };
        self.apply_changes(changes, None, window, cx);
    }

    /// Apply changes on the server, then reload. Used by the grid and dialogs.
    pub fn apply_changes(
        &mut self,
        changes: Vec<RowChange>,
        on_success: Option<OnSuccess>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.saving = true;
        self.save_seq += 1;
        let seq = self.save_seq;
        cx.notify();
        let conn = self.conn.clone();
        let object = self.object.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = io(async move { conn.apply(&object, changes).await }).await;
            this.update_in(cx, |this, window, cx| {
                this.saving = false;
                this.last_save = Some((
                    seq,
                    result.as_ref().map(|s| s.to_string()).map_err(dbclaw_drivers::error_message),
                ));
                match result {
                    Ok(summary) => {
                        window.remove_notification1::<SaveError>(cx.entity_id().as_u64() as usize, cx);
                        if let Some(f) = on_success {
                            f(window, cx);
                        }
                        window.push_notification(
                            Notification::success(format!("Saved: {summary}")).autohide(true),
                            cx,
                        );
                        this.table.update(cx, |t, _| t.delegate_mut().discard_changes());
                        this.reload(true, cx);
                    }
                    Err(e) => {
                        let msg = dbclaw_drivers::error_message(&e);
                        window.push_notification(
                            Notification::error(msg)
                                .title("Changes were not saved")
                                .id1::<SaveError>(cx.entity_id().as_u64() as usize)
                                .autohide(false),
                            cx,
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn discard(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| {
            t.delegate_mut().discard_changes();
            cx.notify();
        });
        cx.notify();
    }

    fn insert(&mut self, template_row: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable {
            window.push_notification(Notification::warning("This object is read-only"), cx);
            return;
        }
        let view = cx.entity().downgrade();
        let d = self.table.read(cx).delegate();
        match self.conn.kind() {
            DbKind::Postgres => {
                let template: Option<Vec<Cell>> = template_row.and_then(|r| d.set.rows.get(r).cloned());
                let title: SharedString = format!("Insert into {}", self.title()).into();
                record_dialog::open_row_form(title, self.columns.clone(), template, view, window, cx);
            }
            DbKind::Mongo => {
                let mut doc = template_row
                    .and_then(|r| d.set.documents.as_ref().and_then(|docs| docs.get(r).cloned()))
                    .unwrap_or_default();
                doc.remove("_id");
                record_dialog::open_document_editor(None, doc, view, window, cx);
            }
        }
    }

    fn edit_document(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let d = self.table.read(cx).delegate();
        let Some(doc) = d.set.documents.as_ref().and_then(|docs| docs.get(row)).cloned() else {
            return;
        };
        let key = match self.row_key(row, cx) {
            Ok(k) => k,
            Err(e) => {
                window.push_notification(Notification::error(e), cx);
                return;
            }
        };
        if !self.editable {
            window.push_notification(Notification::info("Read-only: changes can't be saved"), cx);
        }
        let view = cx.entity().downgrade();
        record_dialog::open_document_editor(Some(key), doc, view, window, cx);
    }

    fn copy_cell(&self, cx: &mut App) {
        if let Some((r, c)) = self.selected_cell(cx) {
            if let Some(text) = self.table.read(cx).delegate().cell_value_text(r, c) {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        } else if let Some(r) = self.selected_row(cx) {
            self.copy_row_json(r, cx);
        }
    }

    fn copy_row_json(&self, row: usize, cx: &mut App) {
        let d = self.table.read(cx).delegate();
        let text = if let Some(doc) = d.set.documents.as_ref().and_then(|docs| docs.get(row)) {
            mongo_shell::format_document(doc, true)
        } else {
            let mut map = serde_json::Map::new();
            for (i, c) in d.set.columns.iter().enumerate() {
                let v = match d.cell(row, i) {
                    Some(Cell::Value(s, _)) => serde_json::Value::String(s.to_string()),
                    _ => serde_json::Value::Null,
                };
                map.insert(c.name.clone(), v);
            }
            serde_json::to_string_pretty(&map).unwrap_or_default()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn row_menu(
        &self,
        row: usize,
        menu: gpui_kit::component::menu::PopupMenu,
        view: WeakEntity<TableView>,
        cx: &App,
    ) -> gpui_kit::component::menu::PopupMenu {
        let col = match self.table.read(cx).right_clicked_cell() {
            Some((_, c)) if c > 0 => Some(c - 1),
            _ => None,
        };
        let deleted = self.table.read(cx).delegate().deleted.contains(&row);
        let mongo = self.conn.kind() == DbKind::Mongo;
        let act = |label: &'static str, icon: Lucide, f: RowAction| {
            let view = view.clone();
            PopupMenuItem::new(label).icon(Icon::new(icon)).on_click(move |_, window, cx| {
                let f = f.clone();
                view.update(cx, |this, cx| f(this, window, cx)).ok();
            })
        };
        let mut menu = menu;
        if let Some(col) = col {
            menu = menu
                .item(act("Edit cell", Lucide::Pencil, Rc::new(move |t, w, cx| t.start_edit(row, col, w, cx))))
                .item(act("Copy value", Lucide::Copy, Rc::new(move |t, _, cx| {
                    if let Some(text) = t.table.read(cx).delegate().cell_value_text(row, col) {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                    }
                })))
                .item(act("Filter by this value", Lucide::ListFilter, Rc::new(move |t, w, cx| t.filter_by_cell(row, col, w, cx))))
                .item(act("Set NULL", Lucide::Ban, Rc::new(move |t, w, cx| t.set_null(row, col, w, cx))));
            if mongo {
                menu = menu.item(act("Remove field", Lucide::Eraser, Rc::new(move |t, w, cx| t.unset_field(row, col, w, cx))));
            }
            menu = menu.separator();
        }
        if mongo {
            menu = menu.item(act("Edit document…", Lucide::Braces, Rc::new(move |t, w, cx| t.edit_document(row, w, cx))));
        }
        menu.item(act("Copy row as JSON", Lucide::Copy, Rc::new(move |t, _, cx| t.copy_row_json(row, cx))))
            .item(act("Duplicate row…", Lucide::CopyPlus, Rc::new(move |t, w, cx| t.insert(Some(row), w, cx))))
            .separator()
            .item(act(
                if deleted { "Undo delete" } else { "Delete row" },
                if deleted { Lucide::Undo2 } else { Lucide::Trash },
                Rc::new(move |t, w, cx| t.toggle_delete(row, w, cx)),
            ))
    }

    fn go_to_page(&mut self, page: u64, cx: &mut Context<Self>) {
        if page != self.page {
            self.page = page;
            self.reload(false, cx);
        }
    }

    fn last_page(&self) -> Option<u64> {
        self.total.map(|t| t.saturating_sub(1) / self.page_size)
    }

    // ---- actions -------------------------------------------------------

    fn on_save(&mut self, _: &SaveChanges, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.reload(true, cx);
    }

    fn on_focus_filter(&mut self, _: &FocusFilter, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_input.update(cx, |s, cx| s.focus(window, cx));
    }

    fn on_delete_row(&mut self, _: &DeleteRow, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.selected_row(cx) {
            self.toggle_delete(r, window, cx);
        }
    }

    fn on_insert_row(&mut self, _: &InsertRow, window: &mut Window, cx: &mut Context<Self>) {
        self.insert(None, window, cx);
    }

    fn on_edit_cell(&mut self, _: &EditCell, window: &mut Window, cx: &mut Context<Self>) {
        if self.table.read(cx).delegate().editor.is_some() {
            return;
        }
        if let Some((r, c)) = self.selected_cell(cx) {
            self.start_edit(r, c, window, cx);
        } else if let Some(r) = self.selected_row(cx) {
            if self.conn.kind() == DbKind::Mongo {
                self.edit_document(r, window, cx);
            } else if !self.table.read(cx).delegate().set.columns.is_empty() {
                self.table.update(cx, |t, cx| t.set_selected_cell(r, 1, cx));
                self.start_edit(r, 0, window, cx);
            }
        }
    }

    fn on_set_null(&mut self, _: &SetNull, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.selected_cell(cx) {
            self.set_null(r, c, window, cx);
        }
    }

    fn on_copy(&mut self, _: &CopyValue, _: &mut Window, cx: &mut Context<Self>) {
        self.copy_cell(cx);
    }

    fn on_cancel(&mut self, _: &CancelEdit, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_edit(window, cx);
    }

    /// Left/Right on a selected row moves into its first cell (the table
    /// would otherwise switch to column selection).
    fn row_to_cell(&mut self, cx: &mut Context<Self>) {
        if let TableSelection::Row(r) = self.table.read(cx).selection()
            && !self.table.read(cx).delegate().set.columns.is_empty()
        {
            self.table.update(cx, |t, cx| t.set_selected_cell(r, 1, cx));
            cx.stop_propagation();
        }
    }

    fn on_next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        if self.last_page().is_none_or(|l| self.page < l) {
            self.go_to_page(self.page + 1, cx);
        }
    }

    fn on_prev_page(&mut self, _: &PrevPage, _: &mut Window, cx: &mut Context<Self>) {
        if self.page > 0 {
            self.go_to_page(self.page - 1, cx);
        }
    }
}

/// An example filter built from the table's own columns.
fn filter_placeholder(columns: &[ColumnMeta]) -> SharedString {
    use dbclaw_core::model::ValueKind;
    let ident = |c: &&ColumnMeta| c.name.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
    let number = columns.iter().filter(ident).find(|c| c.kind == ValueKind::Number && !c.is_pk);
    let text = columns.iter().filter(ident).find(|c| c.kind == ValueKind::Text && !c.is_pk);
    let mut parts = Vec::new();
    if let Some(c) = number {
        parts.push(format!("{} > 100", c.name));
    }
    if let Some(c) = text {
        parts.push(format!("{} ~ \"abc\"", c.name));
    }
    if parts.is_empty()
        && let Some(c) = columns.iter().find(ident) {
            parts.push(format!("{} is not null", c.name));
        }
    if parts.is_empty() {
        "Filter…  (? for syntax)".into()
    } else {
        format!("Filter, e.g. {}   (? for syntax)", parts.join(" and ")).into()
    }
}

fn quote_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Focusable for TableView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TableView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(hint) = self.pending_placeholder.take() {
            self.filter_input.update(cx, |s, cx| s.set_placeholder(hint, window, cx));
        }
        let theme = cx.theme().clone();
        let d = self.table.read(cx).delegate();
        let pending = d.change_count();
        let rows_on_page = d.set.rows.len() as u64;
        let first = self.page * self.page_size;
        let last_page = self.last_page();
        let has_next = match last_page {
            Some(l) => self.page < l,
            None => rows_on_page == self.page_size,
        };
        let range_label: SharedString = if rows_on_page == 0 {
            "0 rows".into()
        } else {
            format!("{}–{}", group_digits(first + 1), group_digits(first + rows_on_page)).into()
        };
        let total_label: SharedString = match self.total {
            Some(t) => format!("of {}", group_digits(t)).into(),
            None if self.counting => "of …".into(),
            None => "".into(),
        };
        let page_size = self.page_size;

        let toolbar = h_flex()
            .gap_1()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div().flex_1().min_w(px(200.)).child(
                    Input::new(&self.filter_input)
                        .small()
                        .prefix(Icon::new(Lucide::ListFilter).small().text_color(theme.muted_foreground))
                        .cleanable(true)
                        .when(self.filter_error.is_some(), |i| i.bordered(true)),
                ),
            )
            .child(
                Popover::new("filter-help")
                    .trigger(Button::new("help").ghost().small().icon(Icon::new(Lucide::CircleQuestionMark)).tooltip("Filter syntax"))
                    .content(|_, _, cx| {
                        div()
                            .p_2()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .whitespace_normal()
                            .max_w(px(520.))
                            .children(filter::HELP.lines().map(|l| div().child(SharedString::from(l.to_string()))))
                    }),
            )
            .child(div().w(px(1.)).h(px(18.)).bg(theme.border).mx_1())
            .child(
                Button::new("refresh")
                    .ghost()
                    .small()
                    .icon(Icon::new(Lucide::RefreshCw))
                    .tooltip("Refresh (⌘R)")
                    .loading(self.loading)
                    .on_click(cx.listener(|this, _, _, cx| this.reload(true, cx))),
            )
            .when(self.editable, |this| {
                this.child(
                    Button::new("insert")
                        .ghost()
                        .small()
                        .icon(Icon::new(Lucide::Plus))
                        .tooltip(if self.conn.kind() == DbKind::Mongo { "Insert document (⌘I)" } else { "Insert row (⌘I)" })
                        .on_click(cx.listener(|this, _, window, cx| this.insert(None, window, cx))),
                )
                .child(
                    Button::new("delete")
                        .ghost()
                        .small()
                        .icon(Icon::new(Lucide::Trash))
                        .tooltip("Mark selected row for deletion (⌘⌫)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(r) = this.selected_row(cx) {
                                this.toggle_delete(r, window, cx);
                            }
                        })),
                )
            })
            .when(pending > 0, |this| {
                this.child(
                    Button::new("discard")
                        .ghost()
                        .small()
                        .icon(Icon::new(Lucide::Undo2))
                        .label("Discard")
                        .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
                )
                .child(
                    Button::new("save")
                        .primary()
                        .small()
                        .icon(Icon::new(Lucide::Save))
                        .label(format!("Save {pending} change{}", if pending == 1 { "" } else { "s" }))
                        .tooltip("Save changes (⌘S)")
                        .loading(self.saving)
                        .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                )
            });

        let filter_error = self.filter_error.clone().map(|e| {
            h_flex()
                .px_2()
                .py_1()
                .gap_1()
                .text_xs()
                .bg(theme.danger.opacity(0.08))
                .text_color(theme.danger)
                .child(Icon::new(Lucide::CircleAlert).xsmall())
                .child(e)
        });

        let body = if let Some(err) = self.error.clone() {
            v_flex()
                .flex_1()
                .p_4()
                .gap_2()
                .child(h_flex().gap_2().text_color(theme.danger).child(Icon::new(Lucide::CircleAlert)).child("Query failed"))
                .child(
                    div()
                        .text_sm()
                        .font_family(theme.mono_font_family.clone())
                        .whitespace_normal()
                        .child(err),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .child(DataTable::new(&self.table).stripe(true).bordered(false))
                .into_any_element()
        };

        let footer = h_flex()
            .gap_2()
            .px_2()
            .h(px(30.))
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(
                Button::new("first")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ChevronsLeft))
                    .disabled(self.page == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.go_to_page(0, cx))),
            )
            .child(
                Button::new("prev")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ChevronLeft))
                    .tooltip("Previous page (⌘[)")
                    .disabled(self.page == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = this.page.saturating_sub(1);
                        this.go_to_page(p, cx)
                    })),
            )
            .child(div().child(range_label))
            .child(div().child(total_label))
            .child(
                Button::new("next")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ChevronRight))
                    .tooltip("Next page (⌘])")
                    .disabled(!has_next)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let p = this.page + 1;
                        this.go_to_page(p, cx)
                    })),
            )
            .child(
                Button::new("last")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::ChevronsRight))
                    .disabled(last_page.is_none_or(|l| self.page >= l))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(l) = this.last_page() {
                            this.go_to_page(l, cx)
                        }
                    })),
            )
            .child(
                Popover::new("page-size")
                    .trigger(Button::new("ps").ghost().xsmall().label(format!("{page_size} / page")).dropdown_caret(true))
                    .content({
                        let view = cx.entity().downgrade();
                        move |_, _, _| {
                            let view = view.clone();
                            v_flex().p_1().children(PAGE_SIZES.iter().map(move |&n| {
                                let view = view.clone();
                                Button::new(("size", n as usize))
                                    .ghost()
                                    .small()
                                    .label(format!("{n} rows"))
                                    .on_click(move |_, _, cx| {
                                        view.update(cx, |this, cx| {
                                            this.page_size = n;
                                            this.page = 0;
                                            this.reload(false, cx);
                                        })
                                        .ok();
                                    })
                            }))
                        }
                    }),
            )
            .child(div().flex_1())
            .when(!self.editable, |this| {
                this.child(h_flex().gap_1().child(Icon::new(Lucide::Eye).xsmall()).child("read-only"))
            })
            .when_some(self.elapsed.filter(|_| !self.loading), |this, e| {
                this.child(div().child(SharedString::from(format_duration(e))))
            })
            .child(div().child(self.conn_name.clone()));

        let _ = window;
        v_flex()
            .id("table-view")
            .key_context("TableView")
            .track_focus(&self.focus)
            .size_full()
            .capture_action(cx.listener(|this, _: &gpui_kit::base::actions::SelectNextColumn, _, cx| this.row_to_cell(cx)))
            .capture_action(cx.listener(|this, _: &gpui_kit::base::actions::SelectPrevColumn, _, cx| this.row_to_cell(cx)))
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_focus_filter))
            .on_action(cx.listener(Self::on_delete_row))
            .on_action(cx.listener(Self::on_insert_row))
            .on_action(cx.listener(Self::on_edit_cell))
            .on_action(cx.listener(Self::on_set_null))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cancel))
            .on_action(cx.listener(Self::on_next_page))
            .on_action(cx.listener(Self::on_prev_page))
            .child(toolbar)
            .children(filter_error)
            .child(body)
            .child(footer)
    }
}

// ---- automation API (used by the control server) ---------------------------

impl TableView {
    /// Data is loading or changes are being saved.
    pub fn is_busy(&self) -> bool {
        self.loading || self.saving
    }

    pub fn last_save(&self) -> Option<&(u64, Result<String, String>)> {
        self.last_save.as_ref()
    }

    pub fn filter_error(&self) -> Option<SharedString> {
        self.filter_error.clone()
    }

    pub fn set_filter_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_input.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.apply_filter(window, cx);
    }

    pub fn sort_by(&mut self, column: Option<String>, desc: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if let Some(c) = &column {
            let known = self.table.read(cx).delegate().set.columns.iter().any(|m| &m.name == c)
                || self.columns.iter().any(|m| &m.name == c);
            if !known && self.conn.kind() == DbKind::Postgres {
                return Err(format!("unknown column `{c}`"));
            }
        }
        let sort = column.map(|c| (c, if desc { ColumnSort::Descending } else { ColumnSort::Ascending }));
        self.set_sort(sort, window, cx);
        Ok(())
    }

    pub fn set_paging(&mut self, page: Option<u64>, page_size: Option<u64>, cx: &mut Context<Self>) {
        if let Some(ps) = page_size.filter(|p| *p > 0) {
            self.page_size = ps.min(100_000);
            if page.is_none() {
                self.page = 0;
            }
        }
        if let Some(p) = page {
            self.page = p;
        }
        self.reload(page_size.is_some(), cx);
    }

    fn column_index(&self, column: &str, cx: &App) -> Result<usize, String> {
        let cols = &self.table.read(cx).delegate().set.columns;
        cols.iter()
            .position(|c| c.name == column)
            .or_else(|| cols.iter().position(|c| c.name.eq_ignore_ascii_case(column)))
            .ok_or_else(|| {
                let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
                format!("unknown column `{column}`; columns: {}", names.join(", "))
            })
    }

    fn check_row(&self, row: usize, cx: &App) -> Result<(), String> {
        let n = self.table.read(cx).delegate().set.rows.len();
        if row >= n {
            return Err(format!("row {row} is out of range (page has {n} rows)"));
        }
        Ok(())
    }

    /// Stage a cell edit; `None` sets NULL.
    pub fn stage_edit(&mut self, row: usize, column: &str, value: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        self.check_row(row, cx)?;
        let col = self.column_index(column, cx)?;
        self.can_edit_cell(row, col, cx).map_err(str::to_string)?;
        match value {
            None => self.set_null(row, col, window, cx),
            Some(text) => {
                let pending = self.pending_for(row, col, text, cx)?;
                self.table.update(cx, |t, cx| {
                    let d = t.delegate_mut();
                    match pending {
                        Some(p) => d.edits.insert((row, col), p),
                        None => d.edits.remove(&(row, col)),
                    };
                    cx.notify();
                });
                cx.notify();
            }
        }
        Ok(())
    }

    pub fn mark_deleted(&mut self, row: usize, deleted: bool, cx: &mut Context<Self>) -> Result<(), String> {
        self.check_row(row, cx)?;
        if !self.editable {
            return Err("this tab is read-only".into());
        }
        self.table.update(cx, |t, cx| {
            let d = t.delegate_mut();
            if deleted {
                d.deleted.insert(row);
            } else {
                d.deleted.remove(&row);
            }
            cx.notify();
        });
        cx.notify();
        Ok(())
    }

    /// Start saving staged changes. Returns the save sequence number to wait
    /// for, or `None` when there was nothing to save.
    pub fn start_save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<Option<u64>, String> {
        if !self.has_changes(cx) {
            return Ok(None);
        }
        let changes = self.collect_changes(cx)?;
        self.apply_changes(changes, None, window, cx);
        Ok(Some(self.save_seq))
    }

    /// Insert a row (Postgres, column → text|null) or document (Mongo).
    pub fn start_insert(&mut self, values: Option<&serde_json::Map<String, serde_json::Value>>, document: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Result<u64, String> {
        if !self.editable {
            return Err("this tab is read-only".into());
        }
        let change = match self.conn.kind() {
            DbKind::Postgres => {
                let values = values.ok_or("`values` (column → value) is required for SQL tables")?;
                for k in values.keys() {
                    if !self.columns.iter().any(|c| &c.name == k) {
                        return Err(format!("unknown column `{k}`"));
                    }
                }
                let vals = self
                    .columns
                    .iter()
                    .map(|c| {
                        let v = match values.get(&c.name) {
                            None => NewValue::Default,
                            Some(serde_json::Value::Null) => NewValue::Text(None),
                            Some(serde_json::Value::String(s)) => NewValue::Text(Some(s.clone())),
                            Some(other) => NewValue::Text(Some(other.to_string())),
                        };
                        (c.name.clone(), v)
                    })
                    .collect();
                RowChange::Insert { values: vals }
            }
            DbKind::Mongo => {
                let text = document.ok_or("`document` is required for collections")?;
                record_dialog::build_change(text, None)?
            }
        };
        self.apply_changes(vec![change], None, window, cx);
        Ok(self.save_seq)
    }

    pub fn start_replace_document(&mut self, row: usize, document: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<u64, String> {
        if self.conn.kind() != DbKind::Mongo {
            return Err("replace_document is for MongoDB collections".into());
        }
        if !self.editable {
            return Err("this tab is read-only".into());
        }
        self.check_row(row, cx)?;
        let key = self.row_key(row, cx)?;
        let change = record_dialog::build_change(document, Some(key))?;
        self.apply_changes(vec![change], None, window, cx);
        Ok(self.save_seq)
    }

    /// JSON description of the tab, with up to `limit` rows from `offset`.
    pub fn snapshot(&self, offset: usize, limit: usize, cx: &App) -> serde_json::Value {
        use serde_json::json;
        let d = self.table.read(cx).delegate();
        let mut tab = json!({
            "kind": "table",
            "connection": self.conn_name.to_string(),
            "connection_id": self.conn_id,
            "database": self.object.database,
            "schema": self.object.schema,
            "name": self.object.name,
            "object_kind": format!("{:?}", self.object.kind),
            "editable": self.editable,
            "filter": self.filter_input.read(cx).value().to_string(),
            "filter_error": self.filter_error.as_ref().map(|s| s.to_string()),
            "sort": self.sort.iter().map(|s| json!({"column": s.column, "dir": if s.dir == SortDir::Asc { "asc" } else { "desc" }})).collect::<Vec<_>>(),
            "page": self.page,
            "page_size": self.page_size,
            "first_row_number": self.page * self.page_size + 1,
            "rows_on_page": d.set.rows.len(),
            "total_rows": self.total,
            "loading": self.loading,
            "saving": self.saving,
            "error": self.error.as_ref().map(|s| s.to_string()),
            "elapsed_ms": self.elapsed.map(|e| e.as_secs_f64() * 1000.0),
            "pending_changes": d.change_count(),
            "last_save": self.last_save.as_ref().map(|(_, r)| match r { Ok(s) => json!({"ok": s}), Err(e) => json!({"error": e}) }),
        });
        tab["grid"] = grid_json(d, offset, limit);
        tab
    }
}

/// Columns + rows of a grid as JSON (shared with the console).
pub fn grid_json(d: &GridDelegate, offset: usize, limit: usize) -> serde_json::Value {
    use serde_json::json;
    let columns: Vec<serde_json::Value> = d
        .set
        .columns
        .iter()
        .map(|c| json!({"name": c.name, "type": c.type_name, "pk": c.is_pk}))
        .collect();
    let end = (offset + limit).min(d.set.rows.len());
    let rows: Vec<serde_json::Value> = (offset.min(end)..end)
        .map(|r| {
            let cells: Vec<serde_json::Value> = (0..d.set.columns.len())
                .map(|c| match d.cell(r, c) {
                    Some(Cell::Value(s, _)) => serde_json::Value::String(s.to_string()),
                    _ => serde_json::Value::Null,
                })
                .collect();
            let edited: Vec<&str> = (0..d.set.columns.len())
                .filter(|c| d.edits.contains_key(&(r, *c)))
                .map(|c| d.set.columns[c].name.as_str())
                .collect();
            let mut row = json!({"row": r, "cells": cells});
            if d.deleted.contains(&r) {
                row["deleted"] = json!(true);
            }
            if !edited.is_empty() {
                row["edited"] = json!(edited);
            }
            row
        })
        .collect();
    json!({"columns": columns, "rows": rows, "offset": offset, "truncated": d.set.truncated})
}
