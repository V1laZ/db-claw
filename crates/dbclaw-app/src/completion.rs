//! Autocompletion providers for the editors: SQL console, Mongo console and
//! the filter bar. The logic lives in `dbclaw_core::complete`; this adapts it
//! to the editor's (LSP-style) completion menu.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::Result;
use dbclaw_core::complete::{self, Catalog, Completions, FilterField, MongoNeed, SuggestionKind};
use dbclaw_core::model::{DbKind, ValueKind};
use dbclaw_drivers::Connection;
use gpui_kit::component::input::CompletionProvider;
use gpui_kit::component::{Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    CompletionTriggerKind, TextEdit,
};

use crate::util::io;

fn item_kind(kind: SuggestionKind) -> CompletionItemKind {
    match kind {
        SuggestionKind::Keyword => CompletionItemKind::KEYWORD,
        SuggestionKind::Function => CompletionItemKind::FUNCTION,
        SuggestionKind::Schema => CompletionItemKind::MODULE,
        SuggestionKind::Table | SuggestionKind::Collection => CompletionItemKind::STRUCT,
        SuggestionKind::View => CompletionItemKind::INTERFACE,
        SuggestionKind::Column => CompletionItemKind::FIELD,
        SuggestionKind::Method => CompletionItemKind::METHOD,
        SuggestionKind::Operator => CompletionItemKind::OPERATOR,
        SuggestionKind::Value => CompletionItemKind::VALUE,
    }
}

fn to_response(rope: &Rope, c: Completions) -> CompletionResponse {
    let range = lsp_types::Range {
        start: rope.offset_to_position(c.replace.start),
        end: rope.offset_to_position(c.replace.end),
    };
    CompletionResponse::Array(
        c.items
            .into_iter()
            .map(|s| CompletionItem {
                label: s.label,
                kind: Some(item_kind(s.kind)),
                detail: s.detail.filter(|d| !d.is_empty()),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text: s.insert })),
                ..Default::default()
            })
            .collect(),
    )
}

fn invoked(trigger: &CompletionContext) -> bool {
    trigger.trigger_kind == CompletionTriggerKind::INVOKED
}

/// Ask for completions while typing (not for large pastes).
fn typing(new_text: &str) -> bool {
    !new_text.is_empty() && new_text.chars().count() <= 2 && !new_text.contains('\n')
}

/// Shared, lazily filled catalog of one database.
#[derive(Clone, Default)]
pub struct SharedCatalog(Rc<RefCell<Catalog>>);

impl SharedCatalog {
    /// Load in the background; completions use whatever is loaded so far.
    pub fn load(conn: Connection, database: String, cx: &mut App) -> Self {
        let shared = SharedCatalog::default();
        let target = shared.0.clone();
        cx.spawn(async move |_| {
            if let Ok(catalog) = io(async move { conn.catalog(&database).await }).await {
                *target.borrow_mut() = catalog;
            }
        })
        .detach();
        shared
    }
}

// ---- SQL console -------------------------------------------------------------

pub struct SqlCompletion {
    pub catalog: SharedCatalog,
}

impl CompletionProvider for SqlCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let s = text.to_string();
        let c = complete::sql(&s, offset, &self.catalog.0.borrow(), invoked(&trigger));
        Task::ready(Ok(to_response(text, c)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        typing(new_text)
    }
}

// ---- Mongo console -------------------------------------------------------------

/// Field paths per collection, filled on demand.
pub type FieldCache = Rc<RefCell<HashMap<String, Vec<(String, ValueKind)>>>>;

pub struct MongoCompletion {
    pub conn: Connection,
    pub database: String,
    pub catalog: SharedCatalog,
    pub default_collection: Option<String>,
    pub fields: FieldCache,
}

impl MongoCompletion {
    fn compute(&self, text: &str, offset: usize, invoked: bool) -> (Completions, Option<MongoNeed>) {
        let collections: Vec<String> = self.catalog.0.borrow().tables.iter().map(|t| t.name.clone()).collect();
        complete::mongo_shell(
            text,
            offset,
            &collections,
            &self.fields.borrow(),
            self.default_collection.as_deref(),
            invoked,
        )
    }
}

impl CompletionProvider for MongoCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let s = text.to_string();
        let invoked = invoked(&trigger);
        let (c, need) = self.compute(&s, offset, invoked);
        let Some(MongoNeed::Fields(coll)) = need else {
            return Task::ready(Ok(to_response(text, c)));
        };
        // Fetch the collection's field names once (cached sample), then answer.
        let (conn, db, fields) = (self.conn.clone(), self.database.clone(), self.fields.clone());
        let this = MongoCompletion {
            conn: self.conn.clone(),
            database: self.database.clone(),
            catalog: self.catalog.clone(),
            default_collection: self.default_collection.clone(),
            fields: self.fields.clone(),
        };
        let rope = text.clone();
        cx.spawn(async move |_| {
            let coll2 = coll.clone();
            let paths = io(async move { conn.field_paths(&db, &coll2).await }).await.unwrap_or_default();
            fields.borrow_mut().insert(coll, paths);
            let (c, _) = this.compute(&s, offset, invoked);
            Ok(to_response(&rope, c))
        })
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        typing(new_text)
    }
}

// ---- Filter bar ----------------------------------------------------------------

pub struct FilterCompletion {
    pub kind: DbKind,
    pub fields: Rc<RefCell<Vec<FilterField>>>,
}

impl CompletionProvider for FilterCompletion {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        _: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let s = text.to_string();
        let c = complete::filter(&s, offset, &self.fields.borrow(), self.kind, invoked(&trigger));
        Task::ready(Ok(to_response(text, c)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        typing(new_text)
    }
}
