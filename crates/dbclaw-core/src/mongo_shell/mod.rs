//! Mongo shell-syntax parser and BSON formatting.
//!
//! * [`parse_value`] / [`parse_document`] — relaxed JSON / JavaScript object
//!   literals (unquoted keys, single quotes, comments, trailing commas,
//!   `ObjectId(…)`, `ISODate(…)`, regex literals, Extended JSON wrappers …).
//! * [`parse_command`] / [`parse_filter_or_command`] — mongosh statements
//!   such as `db.users.find({age: {$gt: 30}}).sort({name: 1}).limit(20)`.
//! * [`format_literal`] / [`format_document`] / [`format_cell`] — BSON back to
//!   the same relaxed syntax; `format_literal` output round-trips through
//!   `parse_value`.
//! * [`parse_cell_edit`] — interpret an inline grid-cell edit.

mod command;
mod format;
mod lexer;
mod literal;

#[cfg(test)]
mod tests;

use std::fmt;
use std::ops::Range;

pub use command::{MongoCommand, UpdateSpec, parse_command, parse_filter_or_command};
pub use format::{format_cell, format_document, format_literal, parse_cell_edit};
pub use literal::{parse_document, parse_value};

/// A parse error with a byte range into the input that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellError {
    pub message: String,
    pub span: Range<usize>,
}

impl ShellError {
    pub fn new(message: impl Into<String>, span: Range<usize>) -> Self {
        ShellError {
            message: message.into(),
            span,
        }
    }

    /// 1-based (line, column) of the start of the span within `input`
    /// (column counted in characters).
    pub fn line_col(&self, input: &str) -> (usize, usize) {
        let end = self.span.start.min(input.len());
        let before = input.get(..end).unwrap_or(input);
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        (line, col)
    }
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ShellError {}
