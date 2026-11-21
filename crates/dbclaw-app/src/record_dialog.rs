//! Dialogs for inserting rows (SQL tables) and editing / inserting whole
//! documents (Mongo). Both save immediately and stay open on failure.

use std::rc::Rc;

use bson::{Bson, Document};
use dbclaw_core::model::{Cell, ColumnMeta, NewValue, RowChange, RowKey};
use dbclaw_core::mongo_shell;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::table_view::TableView;

// ---- SQL row form ------------------------------------------------------

pub struct RowForm {
    columns: Vec<ColumnMeta>,
    inputs: Vec<Entity<InputState>>,
}

impl RowForm {
    fn new(columns: Vec<ColumnMeta>, template: Option<Vec<Cell>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let inputs = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let placeholder = if c.generated {
                    "generated".to_string()
                } else if let Some(d) = &c.default {
                    format!("DEFAULT {d}")
                } else if c.nullable {
                    "NULL".to_string()
                } else {
                    "required".to_string()
                };
                // Don't copy keys / defaults (e.g. serial ids) when duplicating.
                let value = template
                    .as_ref()
                    .filter(|_| !(c.generated || (c.is_pk && c.default.is_some())))
                    .and_then(|t| t.get(i))
                    .and_then(|cell| cell.as_str())
                    .unwrap_or_default()
                    .to_string();
                cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(value)
                })
            })
            .collect();
        Self { columns, inputs }
    }

    /// Empty input → DEFAULT when the column has one, else NULL when
    /// nullable, else the empty string.
    fn values(&self, cx: &App) -> Vec<(String, NewValue)> {
        self.columns
            .iter()
            .zip(&self.inputs)
            .map(|(c, input)| {
                let text = input.read(cx).value().to_string();
                let v = if c.generated {
                    NewValue::Default
                } else if text.is_empty() {
                    if c.default.is_some() {
                        NewValue::Default
                    } else if c.nullable {
                        NewValue::Text(None)
                    } else {
                        NewValue::Text(Some(String::new()))
                    }
                } else {
                    NewValue::Text(Some(text))
                };
                (c.name.clone(), v)
            })
            .collect()
    }
}

impl Render for RowForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .id("row-form")
            .gap_2()
            .max_h(px(520.))
            .overflow_y_scroll()
            .pr_2()
            .children(self.columns.iter().zip(&self.inputs).map(|(c, input)| {
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(
                        v_flex()
                            .w(px(170.))
                            .flex_shrink_0()
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_sm()
                                    .font_medium()
                                    .when(c.is_pk, |t| t.child(Icon::new(Lucide::KeyRound).xsmall().text_color(theme.warning)))
                                    .child(div().truncate().child(c.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(format!("{}{}", c.type_name, if c.nullable { "" } else { " · not null" })),
                            ),
                    )
                    .child(div().flex_1().child(Input::new(input).small()))
            }))
    }
}

pub fn open_row_form(
    columns: Vec<ColumnMeta>,
    template: Option<Vec<Cell>>,
    view: WeakEntity<TableView>,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = view
        .upgrade()
        .map(|v| format!("Insert into {}", v.read(cx).title()))
        .unwrap_or_default()
        .into();
    let form = cx.new(|cx| RowForm::new(columns, template, window, cx));
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let f = form.read(cx);
            if let Some((_, input)) = f.columns.iter().zip(&f.inputs).find(|(c, _)| !c.generated && c.default.is_none()) {
                input.focus_handle(cx).focus(window, cx);
            }
        }
        let form2 = form.clone();
        let view = view.clone();
        dialog
            .title(title.clone())
            .w(px(640.))
            .child(form.clone())
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Empty fields use the column default, or NULL."),
                    )
                    .child(Button::new("cancel").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                    .child(Button::new("insert").primary().label("Insert row").on_click(move |_, window, cx| {
                        let values = form2.read(cx).values(cx);
                        view.update(cx, |v, cx| {
                            v.apply_changes(
                                vec![RowChange::Insert { values }],
                                Some(Box::new(|w: &mut Window, cx: &mut App| w.close_dialog(cx))),
                                window,
                                cx,
                            )
                        })
                        .ok();
                    })),
            )
    });
}

// ---- Mongo document editor ---------------------------------------------

pub struct DocumentForm {
    editor: Entity<EditorState>,
    error: Option<SharedString>,
}

impl Render for DocumentForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .child(Editor::new(&self.editor).h(px(440.)))
            .when_some(self.error.clone(), |this, e| {
                this.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(Icon::new(Lucide::CircleAlert).xsmall())
                        .child(e),
                )
            })
    }
}

pub fn open_document_editor(
    key: Option<RowKey>,
    document: Document,
    view: WeakEntity<TableView>,
    window: &mut Window,
    cx: &mut App,
) {
    let text = if document.is_empty() {
        "{\n  \n}".to_string()
    } else {
        mongo_shell::format_document(&document, true)
    };
    let editor = cx.new(|cx| {
        EditorState::new(window, cx)
            .language("javascript")
            .line_number(true)
            .default_value(text)
    });
    let form = cx.new(|_| DocumentForm { editor, error: None });
    let editing = key.is_some();
    let key = Rc::new(key);
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            let editor = form.read(cx).editor.clone();
            editor.focus_handle(cx).focus(window, cx);
        }
        let form2 = form.clone();
        let view = view.clone();
        let key = key.clone();
        dialog
            .title(if editing { "Edit document" } else { "Insert document" })
            .w(px(720.))
            .child(form.clone())
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Relaxed JSON: unquoted keys, ObjectId(…), ISODate(…), NumberLong(…)…"),
                    )
                    .child(Button::new("cancel").label("Cancel").on_click(|_, w, cx| w.close_dialog(cx)))
                    .child(
                        Button::new("save")
                            .primary()
                            .label(if editing { "Save document" } else { "Insert document" })
                            .on_click(move |_, window, cx| {
                                let text = form2.read(cx).editor.read(cx).value().to_string();
                                let change = build_change(&text, (*key).clone());
                                match change {
                                    Ok(change) => {
                                        view.update(cx, |v, cx| {
                                            v.apply_changes(
                                                vec![change],
                                                Some(Box::new(|w: &mut Window, cx: &mut App| w.close_dialog(cx))),
                                                window,
                                                cx,
                                            )
                                        })
                                        .ok();
                                    }
                                    Err(e) => form2.update(cx, |f, cx| {
                                        f.error = Some(e.into());
                                        cx.notify();
                                    }),
                                }
                            }),
                    ),
            )
    });
}

pub fn build_change(text: &str, key: Option<RowKey>) -> Result<RowChange, String> {
    let mut doc = mongo_shell::parse_document(text).map_err(|e| {
        let (line, col) = e.line_col(text);
        format!("{} (line {line}, column {col})", e.message)
    })?;
    match key {
        Some(RowKey::Id(id)) => {
            match doc.get("_id") {
                None => {
                    // Keep the original id, first.
                    let mut with_id = Document::new();
                    with_id.insert("_id", id.clone());
                    with_id.extend(doc);
                    doc = with_id;
                }
                Some(v) if *v != id => return Err("_id can't be changed".into()),
                Some(_) => {}
            }
            Ok(RowChange::Replace { key: RowKey::Id(id), document: doc })
        }
        Some(_) => Err("unexpected row key".into()),
        None => {
            if matches!(doc.get("_id"), Some(Bson::Null)) {
                doc.remove("_id");
            }
            Ok(RowChange::InsertDocument { document: doc })
        }
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the gpui_kit glob would shadow `#[test]`.
    use super::{Bson, RowChange, RowKey, build_change};
    use bson::{doc, oid::ObjectId};

    #[test]
    fn replace_keeps_id() {
        let id = Bson::ObjectId(ObjectId::new());
        let ch = build_change("{name: 'x'}", Some(RowKey::Id(id.clone()))).unwrap();
        match ch {
            RowChange::Replace { document, .. } => {
                assert_eq!(document.get("_id"), Some(&id));
                assert_eq!(document.keys().next().map(String::as_str), Some("_id"));
            }
            _ => panic!(),
        }
        assert!(build_change("{_id: 5}", Some(RowKey::Id(id))).is_err());
        assert!(matches!(
            build_change("{a: 1}", None).unwrap(),
            RowChange::InsertDocument { document } if document == doc! {"a": 1}
        ));
        assert!(build_change("{a: ", None).unwrap_err().contains("line 1"));
    }
}
