//! The data grid: a `TableDelegate` over a `ResultSet` with staged edits and
//! deletions. Used by both table/collection views and console results.
//!
//! Column 0 is a fixed row-number column; data column `i` is table column
//! `i + 1`.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;

use dbclaw_core::model::{Cell, ColumnMeta, NewValue, ResultSet, ValueKind};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::util::single_line;

/// Horizontal padding inside data cells.
const CELL_PAD: Pixels = px(8.);

/// Max characters drawn per cell (the full value stays in memory).
const CELL_DISPLAY_CHARS: usize = 300;

pub type FollowHandler = Rc<dyn Fn(usize, usize, &mut Window, &mut App)>;
pub type SortHandler = Rc<dyn Fn(Option<(String, ColumnSort)>, &mut Window, &mut App)>;

#[derive(Clone, Debug)]
pub struct PendingCell {
    pub display: Cell,
    pub value: NewValue,
}

pub struct CellEditor {
    pub row: usize,
    pub col: usize,
    pub input: Entity<InputState>,
}

pub struct GridDelegate {
    pub set: ResultSet,
    /// Row number of the first row (for pagination).
    pub row_offset: usize,
    /// Current sort, by data column index.
    pub sort: Option<(usize, ColumnSort)>,
    /// When set, sorting is done by the owner (server side); otherwise rows
    /// are sorted locally.
    pub on_sort: Option<SortHandler>,
    /// Follow the foreign key of a cell: (row, data column).
    pub on_follow: Option<FollowHandler>,
    pub edits: HashMap<(usize, usize), PendingCell>,
    pub deleted: BTreeSet<usize>,
    pub editor: Option<CellEditor>,
    pub loading: bool,
    widths: Vec<Pixels>,
    /// Columns already widened for their reference arrow.
    widened: BTreeSet<usize>,
}

impl GridDelegate {
    pub fn new() -> Self {
        Self {
            set: ResultSet::default(),
            row_offset: 0,
            sort: None,
            on_sort: None,
            on_follow: None,
            edits: HashMap::new(),
            deleted: BTreeSet::new(),
            editor: None,
            loading: false,
            widths: Vec::new(),
            widened: BTreeSet::new(),
        }
    }

    /// Replace the data. Column widths are kept when the columns are the same.
    pub fn set_data(&mut self, set: ResultSet, row_offset: usize) {
        let same_columns = set.columns.len() == self.set.columns.len()
            && set.columns.iter().zip(&self.set.columns).all(|(a, b)| a.name == b.name);
        self.set = set;
        self.row_offset = row_offset;
        self.edits.clear();
        self.deleted.clear();
        self.editor = None;
        if !same_columns || self.widths.len() != self.set.columns.len() {
            self.widths = compute_widths(&self.set);
            self.widened.clear();
        }
    }

    /// Make room for the arrow in reference columns (once per column, so
    /// manual resizing isn't undone on the next page).
    pub fn widen_reference_columns(&mut self) {
        for (i, c) in self.set.columns.iter().enumerate() {
            if c.references.is_some() && self.widened.insert(i)
                && let Some(w) = self.widths.get_mut(i) {
                    *w = (*w + px(20.)).min(px(440.));
                }
        }
    }

    pub fn has_changes(&self) -> bool {
        !self.edits.is_empty() || !self.deleted.is_empty()
    }

    pub fn change_count(&self) -> usize {
        let edited_rows: BTreeSet<usize> = self.edits.keys().map(|(r, _)| *r).collect();
        edited_rows.difference(&self.deleted).count() + self.deleted.len()
    }

    pub fn discard_changes(&mut self) {
        self.edits.clear();
        self.deleted.clear();
        self.editor = None;
    }

    pub fn column_meta(&self, data_col: usize) -> Option<&ColumnMeta> {
        self.set.columns.get(data_col)
    }

    /// Effective cell (pending edit or original).
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if let Some(p) = self.edits.get(&(row, col)) {
            return Some(&p.display);
        }
        self.set.rows.get(row).and_then(|r| r.get(col))
    }

    /// Plain text value of a cell for copying (`NULL` → empty).
    pub fn cell_value_text(&self, row: usize, col: usize) -> Option<String> {
        self.cell(row, col).map(|c| c.as_str().unwrap_or_default().to_string())
    }

    fn sort_locally(&mut self, col: usize, sort: ColumnSort) {
        let mut idx: Vec<usize> = (0..self.set.rows.len()).collect();
        if sort != ColumnSort::Default {
            let numeric = self.set.columns.get(col).map(|c| c.kind) == Some(ValueKind::Number);
            idx.sort_by(|&a, &b| {
                let ca = self.set.rows[a].get(col);
                let cb = self.set.rows[b].get(col);
                let ord = compare_cells(ca, cb, numeric);
                if sort == ColumnSort::Descending { ord.reverse() } else { ord }
            });
        } else {
            return;
        }
        let rows = std::mem::take(&mut self.set.rows);
        let mut rows: Vec<Option<Vec<Cell>>> = rows.into_iter().map(Some).collect();
        self.set.rows = idx.iter().map(|&i| rows[i].take().unwrap()).collect();
        if let Some(docs) = self.set.documents.take() {
            let mut docs: Vec<Option<bson::Document>> = docs.into_iter().map(Some).collect();
            self.set.documents = Some(idx.iter().map(|&i| docs[i].take().unwrap()).collect());
        }
        if let Some(ctids) = self.set.ctids.take() {
            self.set.ctids = Some(idx.iter().map(|&i| ctids[i].clone()).collect());
        }
    }
}

fn compare_cells(a: Option<&Cell>, b: Option<&Cell>, numeric: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.and_then(Cell::as_str), b.and_then(Cell::as_str)) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => {
            if numeric
                && let (Ok(fx), Ok(fy)) = (x.parse::<f64>(), y.parse::<f64>()) {
                    return fx.partial_cmp(&fy).unwrap_or(Ordering::Equal);
                }
            x.cmp(y)
        }
    }
}

/// Estimate column widths from the header and the first rows.
fn compute_widths(set: &ResultSet) -> Vec<Pixels> {
    const CHAR_W: f32 = 7.4;
    set.columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            // Header shows: [key icon] name  type  [sort icon]
            let header = c.name.chars().count()
                + c.type_name.chars().count().min(16)
                + if c.is_pk { 3 } else { 0 }
                + 5;
            let content = set
                .rows
                .iter()
                .take(200)
                .filter_map(|r| r.get(i))
                .map(|cell| match cell {
                    Cell::Value(s, _) => s.chars().take(60).count(),
                    _ => 4,
                })
                .max()
                .unwrap_or(4);
            px((header.max(content) as f32 * CHAR_W + 24.0).clamp(64.0, 420.0))
        })
        .collect()
}

impl TableDelegate for GridDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.set.columns.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        self.set.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        if col_ix == 0 {
            let digits = (self.row_offset + self.set.rows.len()).max(1).to_string().len();
            return Column::new("#", "#")
                .width(px(digits as f32 * 8.0 + 20.0))
                .fixed_left()
                .resizable(false)
                .movable(false)
                .selectable(false)
                .text_right();
        }
        let data_col = col_ix - 1;
        let meta = &self.set.columns[data_col];
        let mut column = Column::new(SharedString::from(format!("c{data_col}")), meta.name.clone())
            .width(self.widths.get(data_col).copied().unwrap_or(px(120.)))
            // No left padding: cells draw it themselves next to the divider.
            .paddings(Edges { top: px(0.), bottom: px(0.), left: px(0.), right: CELL_PAD })
            .movable(false)
            .sortable();
        if let Some((c, sort)) = self.sort
            && c == data_col {
                column = column.sort(sort);
            }
        if meta.kind.is_right_aligned() {
            column = column.text_right();
        }
        column
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if col_ix == 0 {
            return div().into_any_element();
        }
        let meta = &self.set.columns[col_ix - 1];
        h_flex()
            .w_full()
            .h_full()
            .gap_1()
            .overflow_hidden()
            .pl(CELL_PAD)
            .border_l_1()
            .border_color(cx.theme().table_row_border)
            .when(meta.is_pk, |this| {
                this.child(
                    Icon::new(Lucide::KeyRound).xsmall().text_color(cx.theme().warning),
                )
            })
            .when_some(meta.references.clone(), |this, target| {
                this.child(
                    div()
                        .id(("fk-head", col_ix))
                        .child(Icon::new(Lucide::Link).xsmall().text_color(cx.theme().link))
                        .tooltip(move |window, cx| {
                            Tooltip::new(format!("References {target}")).build(window, cx)
                        }),
                )
            })
            .child(div().font_semibold().truncate().child(meta.name.clone()))
            .when(!meta.type_name.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .truncate()
                        .child(meta.type_name.clone()),
                )
            })
            .into_any_element()
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        if col_ix == 0 {
            return;
        }
        let data_col = col_ix - 1;
        self.sort = (sort != ColumnSort::Default).then_some((data_col, sort));
        match self.on_sort.clone() {
            Some(handler) => {
                let arg = self
                    .sort
                    .map(|(c, s)| (self.set.columns[c].name.clone(), s));
                // The owner re-queries and updates this table; do it outside
                // of the table's own update.
                window.defer(cx, move |window, cx| handler(arg, window, cx));
            }
            None => {
                self.sort_locally(data_col, sort);
                cx.notify();
            }
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let deleted = self.deleted.contains(&row_ix);
        if col_ix == 0 {
            return div()
                .w_full()
                .text_xs()
                .text_right()
                .text_color(if deleted { theme.danger } else { theme.muted_foreground })
                .child(SharedString::from((self.row_offset + row_ix + 1).to_string()))
                .into_any_element();
        }
        let data_col = col_ix - 1;
        if let Some(editor) = &self.editor
            && editor.row == row_ix && editor.col == data_col {
                return div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .border_l_1()
                    .border_color(cx.theme().table_row_border)
                    .child(Input::new(&editor.input).xsmall().appearance(false))
                    .into_any_element();
            }
        let pending = self.edits.contains_key(&(row_ix, data_col));
        let cell = self.cell(row_ix, data_col).cloned().unwrap_or(Cell::Missing);
        let follow = self
            .on_follow
            .clone()
            .filter(|_| self.set.columns.get(data_col).is_some_and(|m| m.references.is_some()));
        let base = div()
            .w_full()
            .h_full()
            .flex()
            .items_center()
            .overflow_hidden()
            // Vertical divider between columns (the cell's left edge).
            .pl(CELL_PAD)
            .border_l_1()
            .border_color(theme.table_row_border)
            .whitespace_nowrap()
            .when(pending, |this| this.bg(theme.warning.opacity(0.18)))
            .when(deleted, |this| this.bg(theme.danger.opacity(0.12)).line_through());
        let el = match cell {
            Cell::Missing => base,
            Cell::Null => base
                .italic()
                .text_color(theme.muted_foreground.opacity(0.7))
                .child("NULL"),
            Cell::Value(text, kind) => {
                let color = match kind {
                    ValueKind::Number => Some(theme.blue),
                    ValueKind::Bool => Some(theme.magenta),
                    ValueKind::DateTime => Some(theme.green),
                    ValueKind::Json | ValueKind::Array => Some(theme.muted_foreground),
                    ValueKind::Uuid => Some(theme.cyan),
                    _ => None,
                };
                let text = SharedString::from(single_line(&text, CELL_DISPLAY_CHARS));
                match follow {
                    // Foreign key: a link arrow on the left opens the referenced row.
                    Some(follow) => base
                        .gap_1()
                        .when_some(color, |this, c| this.text_color(c))
                        .child(
                            div()
                                .id(("fk-link", row_ix * 4096 + data_col))
                                .flex_shrink_0()
                                .cursor_pointer()
                                .rounded_sm()
                                .text_color(theme.link)
                                .hover(|s| s.bg(theme.link.opacity(0.15)))
                                .child(Icon::new(Lucide::ArrowUpRight).xsmall())
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    follow(row_ix, data_col, window, cx);
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .when(kind.is_right_aligned(), |this| this.text_right())
                                .child(text),
                        ),
                    None => base
                        .when(kind.is_right_aligned(), |this| this.justify_end())
                        .when_some(color, |this, c| this.text_color(c))
                        .child(text),
                }
            }
        };
        el.into_any_element()
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if self.loading { "Loading…" } else { "No rows" })
    }

    fn loading(&self, _: &App) -> bool {
        self.loading && self.set.rows.is_empty()
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        if col_ix == 0 {
            return (self.row_offset + row_ix + 1).to_string();
        }
        self.cell_value_text(row_ix, col_ix - 1).unwrap_or_default()
    }

    fn visible_rows_changed(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) {
    }
}
