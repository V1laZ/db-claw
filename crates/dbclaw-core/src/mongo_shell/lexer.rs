//! Low-level scanner shared by the literal and command parsers.
//!
//! The parsers are scannerless recursive-descent parsers: they walk a
//! [`Cursor`] over the input and call the helpers here to skip whitespace
//! and comments and to read identifiers, strings and numbers. All positions
//! are byte offsets into the original input.

use std::ops::Range;

use bson::Bson;

use super::ShellError;

pub(crate) struct Cursor<'a> {
    pub src: &'a str,
    pub pos: usize,
}

pub(crate) fn is_ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}

pub(crate) fn is_ident_continue(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric()
}

/// True when `s` can be written as an unquoted object key / identifier.
pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if is_ident_start(c) => chars.all(is_ident_continue),
        _ => false,
    }
}

impl<'a> Cursor<'a> {
    pub fn new(src: &'a str) -> Self {
        Cursor { src, pos: 0 }
    }

    pub fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    pub fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    /// The character after the current one.
    pub fn peek2(&self) -> Option<char> {
        let mut it = self.rest().chars();
        it.next();
        it.next()
    }

    pub fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    pub fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    pub fn err<T>(&self, message: impl Into<String>, span: Range<usize>) -> Result<T, ShellError> {
        Err(ShellError::new(message, span))
    }

    /// Span covering the current character (or an empty span at the end).
    pub fn here(&self) -> Range<usize> {
        match self.peek() {
            Some(c) => self.pos..self.pos + c.len_utf8(),
            None => self.pos..self.pos,
        }
    }

    /// Describes the current character for error messages.
    pub fn describe_here(&self) -> String {
        match self.peek() {
            None => "end of input".to_string(),
            Some(c) => format!("`{c}`"),
        }
    }

    /// Skips whitespace, `// line` and `/* block */` comments.
    pub fn skip_ws(&mut self) -> Result<(), ShellError> {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() || c == '\u{feff}' => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => {
                    let start = self.pos;
                    self.pos += 2;
                    match self.rest().find("*/") {
                        Some(i) => self.pos += i + 2,
                        None => return self.err("unterminated /* comment", start..self.src.len()),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Skips whitespace and then expects `c`.
    pub fn expect(&mut self, c: char, context: &str) -> Result<usize, ShellError> {
        self.skip_ws()?;
        let start = self.pos;
        if self.eat(c) {
            Ok(start)
        } else {
            self.err(
                format!("expected `{c}` {context}, found {}", self.describe_here()),
                self.here(),
            )
        }
    }

    /// Reads an identifier if one starts here.
    pub fn ident(&mut self) -> Option<(&'a str, Range<usize>)> {
        let start = self.pos;
        match self.peek() {
            Some(c) if is_ident_start(c) => {
                self.bump();
            }
            _ => return None,
        }
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                self.bump();
            } else {
                break;
            }
        }
        Some((&self.src[start..self.pos], start..self.pos))
    }

    /// Peeks the identifier starting here without consuming it.
    pub fn peek_ident(&self) -> Option<&'a str> {
        let mut c = Cursor {
            src: self.src,
            pos: self.pos,
        };
        c.ident().map(|(s, _)| s)
    }

    /// Reads a `"…"` or `'…'` string literal with JavaScript escapes.
    /// The cursor must be on the opening quote.
    pub fn string(&mut self) -> Result<String, ShellError> {
        let start = self.pos;
        let quote = self.bump().expect("string() called on a quote");
        let mut out = String::new();
        loop {
            let c = match self.bump() {
                None => return self.err("unterminated string literal", start..self.src.len()),
                Some(c) => c,
            };
            match c {
                c if c == quote => return Ok(out),
                '\n' | '\r' => {
                    return self.err(
                        "unterminated string literal (strings cannot span lines; use \\n)",
                        start..self.pos - 1,
                    );
                }
                '\\' => self.escape(&mut out, start)?,
                c => out.push(c),
            }
        }
    }

    fn escape(&mut self, out: &mut String, string_start: usize) -> Result<(), ShellError> {
        let esc_start = self.pos - 1;
        let c = match self.bump() {
            None => return self.err("unterminated string literal", string_start..self.src.len()),
            Some(c) => c,
        };
        match c {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' if !matches!(self.peek(), Some('0'..='9')) => out.push('\0'),
            '\r' => {
                // line continuation
                self.eat('\n');
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            'x' => {
                let v = self.hex_digits(2, esc_start)?;
                out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
            }
            'u' => {
                let v = if self.eat('{') {
                    let s = self.pos;
                    while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                        self.bump();
                    }
                    let digits = &self.src[s..self.pos];
                    if digits.is_empty() || !self.eat('}') {
                        return self.err("invalid \\u{…} escape", esc_start..self.pos);
                    }
                    match u32::from_str_radix(digits, 16) {
                        Ok(v) if v <= 0x10ffff => v,
                        _ => return self.err("invalid \\u{…} escape", esc_start..self.pos),
                    }
                } else {
                    self.hex_digits(4, esc_start)?
                };
                if (0xd800..0xdc00).contains(&v) {
                    // high surrogate: try to combine with a following \uDC00-\uDFFF
                    let save = self.pos;
                    if self.rest().starts_with("\\u") {
                        self.pos += 2;
                        if let Ok(lo) = self.hex_digits(4, esc_start)
                            && (0xdc00..0xe000).contains(&lo)
                        {
                            let cp = 0x10000 + ((v - 0xd800) << 10) + (lo - 0xdc00);
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            return Ok(());
                        }
                    }
                    self.pos = save;
                    out.push('\u{fffd}');
                } else {
                    out.push(char::from_u32(v).unwrap_or('\u{fffd}'));
                }
            }
            other => out.push(other),
        }
        Ok(())
    }

    fn hex_digits(&mut self, n: usize, esc_start: usize) -> Result<u32, ShellError> {
        let s = self.pos;
        for _ in 0..n {
            match self.peek() {
                Some(c) if c.is_ascii_hexdigit() => {
                    self.bump();
                }
                _ => {
                    return self.err(
                        format!("invalid escape sequence (expected {n} hex digits)"),
                        esc_start..self.pos,
                    );
                }
            }
        }
        Ok(u32::from_str_radix(&self.src[s..self.pos], 16).expect("hex digits"))
    }

    /// Reads an unsigned number literal (the sign is handled by the caller).
    /// `negative` is applied to the result. Integers become Int32 when they
    /// fit, else Int64, else Double; anything with a fraction or exponent is
    /// a Double.
    pub fn number(&mut self, negative: bool, start: usize) -> Result<Bson, ShellError> {
        let num_start = self.pos;
        let rest = self.rest();
        if rest.starts_with("0x") || rest.starts_with("0X") {
            self.pos += 2;
            let s = self.pos;
            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                self.bump();
            }
            let digits = &self.src[s..self.pos];
            self.check_number_end(start)?;
            let v = match i128::from_str_radix(digits, 16) {
                Ok(v) => v,
                Err(_) if digits.is_empty() => {
                    return self.err("invalid hexadecimal number", start..self.pos);
                }
                Err(_) => return Ok(Bson::Double(f64::INFINITY)),
            };
            return Ok(int_to_bson(if negative { -v } else { v }));
        }
        let mut is_float = false;
        while matches!(self.peek(), Some('0'..='9')) {
            self.bump();
        }
        if self.peek() == Some('.') {
            is_float = true;
            self.bump();
            while matches!(self.peek(), Some('0'..='9')) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.pos;
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if matches!(self.peek(), Some('0'..='9')) {
                is_float = true;
                while matches!(self.peek(), Some('0'..='9')) {
                    self.bump();
                }
            } else {
                self.pos = save;
            }
        }
        let text = &self.src[num_start..self.pos];
        if text.is_empty() || text == "." {
            return self.err("invalid number", start..self.pos.max(start + 1));
        }
        self.check_number_end(start)?;
        if !is_float && let Ok(v) = text.parse::<i128>() {
            return Ok(int_to_bson(if negative { -v } else { v }));
        }
        let mut owned = String::with_capacity(text.len() + 1);
        if negative {
            owned.push('-');
        }
        owned.push_str(text);
        match owned.parse::<f64>() {
            Ok(v) => Ok(Bson::Double(v)),
            Err(_) => self.err("invalid number", start..self.pos),
        }
    }

    fn check_number_end(&self, start: usize) -> Result<(), ShellError> {
        match self.peek() {
            Some(c) if is_ident_continue(c) || c == '.' => {
                let mut end = self.pos;
                for c in self.rest().chars() {
                    if is_ident_continue(c) || c == '.' {
                        end += c.len_utf8();
                    } else {
                        break;
                    }
                }
                self.err(
                    format!("invalid number `{}`", &self.src[start..end]),
                    start..end,
                )
            }
            _ => Ok(()),
        }
    }
}

fn int_to_bson(v: i128) -> Bson {
    if let Ok(v) = i32::try_from(v) {
        Bson::Int32(v)
    } else if let Ok(v) = i64::try_from(v) {
        Bson::Int64(v)
    } else {
        Bson::Double(v as f64)
    }
}
