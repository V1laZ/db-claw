//! Tokenizer for the filter language.

use super::FilterError;
use std::ops::Range;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    /// Bare word (field name, keyword or function name).
    Ident(String),
    /// `` `quoted name` ``.
    QuotedIdent(String),
    Str(String),
    /// Raw numeric text, parsed by the parser (so it can report spans).
    Number(String),
    Regex {
        pattern: String,
        flags: String,
    },
    Op(Op),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Tilde,
    NotTilde,
    StartsWith,
    EndsWith,
    AndAnd,
    OrOr,
    Bang,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub tok: Tok,
    pub span: Range<usize>,
}

const OP_CHARS: &str = "=<>!~^$&|";

pub(crate) fn lex(src: &str) -> Result<Vec<Token>, FilterError> {
    let mut lx = Lexer {
        src,
        pos: 0,
        tokens: Vec::new(),
    };
    lx.run()?;
    Ok(lx.tokens)
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    tokens: Vec<Token>,
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.src[self.pos..].chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn push(&mut self, tok: Tok, start: usize) {
        self.tokens.push(Token {
            tok,
            span: start..self.pos,
        });
    }

    fn err<T>(&self, msg: impl Into<String>, span: Range<usize>) -> Result<T, FilterError> {
        Err(FilterError::new(msg, span))
    }

    fn run(&mut self) -> Result<(), FilterError> {
        while let Some(c) = self.peek() {
            let start = self.pos;
            if c.is_whitespace() {
                self.bump();
                continue;
            }
            match c {
                '(' | ')' | '[' | ']' | ',' => {
                    self.bump();
                    let tok = match c {
                        '(' => Tok::LParen,
                        ')' => Tok::RParen,
                        '[' => Tok::LBracket,
                        ']' => Tok::RBracket,
                        _ => Tok::Comma,
                    };
                    self.push(tok, start);
                }
                '.' => {
                    self.bump();
                    self.push(Tok::Dot, start);
                    // Numeric path segment such as `items.0.name`.
                    if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        let seg_start = self.pos;
                        while self.peek().is_some_and(is_ident_char) {
                            self.bump();
                        }
                        let text = self.src[seg_start..self.pos].to_string();
                        self.push(Tok::Ident(text), seg_start);
                    }
                }
                '"' | '\'' => self.string(c)?,
                '`' => self.quoted_ident()?,
                '/' => self.regex()?,
                '-' if self.peek2().is_some_and(|c| c.is_ascii_digit()) => self.number()?,
                c if c.is_ascii_digit() => self.number()?,
                c if is_ident_start(c) => {
                    while self.peek().is_some_and(is_ident_char) {
                        self.bump();
                    }
                    let text = self.src[start..self.pos].to_string();
                    self.push(Tok::Ident(text), start);
                }
                c if OP_CHARS.contains(c) => self.operator()?,
                c => {
                    let end = start + c.len_utf8();
                    let hint = match c {
                        '\u{201c}' | '\u{201d}' | '\u{201e}' => {
                            "; use plain \"straight\" quotes for text"
                        }
                        '\u{2018}' | '\u{2019}' => "; use plain 'straight' quotes for text",
                        '-' => "; negative numbers are written like -5",
                        _ => "",
                    };
                    return self.err(format!("unexpected character '{c}'{hint}"), start..end);
                }
            }
        }
        Ok(())
    }

    fn string(&mut self, quote: char) -> Result<(), FilterError> {
        let start = self.pos;
        self.bump();
        let mut out = String::new();
        loop {
            let esc_start = self.pos;
            match self.bump() {
                None => return self.err("unclosed string", start..self.pos),
                Some(c) if c == quote => break,
                Some('\\') => match self.bump() {
                    None => return self.err("unclosed string", start..self.pos),
                    Some('"') => out.push('"'),
                    Some('\'') => out.push('\''),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('0') => out.push('\0'),
                    Some('u') => {
                        let c = self.unicode_escape(esc_start)?;
                        out.push(c);
                    }
                    Some(other) => {
                        return self.err(
                            format!("unknown escape '\\{other}'; write '\\\\' for a backslash"),
                            esc_start..self.pos,
                        );
                    }
                },
                Some(c) => out.push(c),
            }
        }
        self.push(Tok::Str(out), start);
        Ok(())
    }

    /// Reads the `XXXX` of a `\uXXXX` escape (the `\u` is already consumed),
    /// combining UTF-16 surrogate pairs.
    fn unicode_escape(&mut self, esc_start: usize) -> Result<char, FilterError> {
        let hi = self.hex4(esc_start)?;
        if (0xD800..0xDC00).contains(&hi) {
            let rest = &self.src[self.pos..];
            if rest.starts_with("\\u") {
                self.pos += 2;
                let lo = self.hex4(esc_start)?;
                if (0xDC00..0xE000).contains(&lo) {
                    let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                    if let Some(c) = char::from_u32(cp) {
                        return Ok(c);
                    }
                }
            }
            return self.err(
                "invalid unicode escape (unpaired surrogate)",
                esc_start..self.pos,
            );
        }
        char::from_u32(hi).map_or_else(
            || {
                self.err(
                    "invalid unicode escape (unpaired surrogate)",
                    esc_start..self.pos,
                )
            },
            Ok,
        )
    }

    fn hex4(&mut self, esc_start: usize) -> Result<u32, FilterError> {
        let mut v = 0u32;
        for _ in 0..4 {
            match self.peek().and_then(|c| c.to_digit(16)) {
                Some(d) => {
                    self.bump();
                    v = v * 16 + d;
                }
                None => {
                    return self.err("expected 4 hex digits after '\\u'", esc_start..self.pos);
                }
            }
        }
        Ok(v)
    }

    fn quoted_ident(&mut self) -> Result<(), FilterError> {
        let start = self.pos;
        self.bump();
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return self.err("unclosed `quoted name`", start..self.pos),
                Some('`') if self.peek() == Some('`') => {
                    self.bump();
                    out.push('`');
                }
                Some('`') => break,
                Some(c) => out.push(c),
            }
        }
        if out.is_empty() {
            return self.err("empty field name", start..self.pos);
        }
        self.push(Tok::QuotedIdent(out), start);
        Ok(())
    }

    fn regex(&mut self) -> Result<(), FilterError> {
        let start = self.pos;
        self.bump();
        let mut pattern = String::new();
        loop {
            match self.bump() {
                None => return self.err("unclosed regex; end it with '/'", start..self.pos),
                Some('/') => break,
                Some('\\') => match self.bump() {
                    None => return self.err("unclosed regex; end it with '/'", start..self.pos),
                    Some('/') => pattern.push('/'),
                    Some(c) => {
                        pattern.push('\\');
                        pattern.push(c);
                    }
                },
                Some(c) => pattern.push(c),
            }
        }
        let mut flags: Vec<char> = Vec::new();
        while let Some(c) = self.peek().filter(|c| c.is_alphanumeric()) {
            self.bump();
            if !"imsx".contains(c) {
                return self.err(
                    format!("unknown regex flag '{c}'; supported flags are i, m, s, x"),
                    self.pos - c.len_utf8()..self.pos,
                );
            }
            if !flags.contains(&c) {
                flags.push(c);
            }
        }
        flags.sort_unstable();
        self.push(
            Tok::Regex {
                pattern,
                flags: flags.into_iter().collect(),
            },
            start,
        );
        Ok(())
    }

    fn number(&mut self) -> Result<(), FilterError> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.bump();
        }
        let digits = |lx: &mut Self| {
            while lx.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                lx.bump();
            }
        };
        digits(self);
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            digits(self);
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let rest = &self.src[self.pos + 1..];
            let mut it = rest.chars();
            let ok = match it.next() {
                Some('+' | '-') => it.next().is_some_and(|c| c.is_ascii_digit()),
                Some(c) => c.is_ascii_digit(),
                None => false,
            };
            if ok {
                self.bump();
                if matches!(self.peek(), Some('+' | '-')) {
                    self.bump();
                }
                digits(self);
            }
        }
        if self.peek().is_some_and(is_ident_char) {
            while self.peek().is_some_and(is_ident_char) {
                self.bump();
            }
            return self.err(
                format!("invalid number '{}'", &self.src[start..self.pos]),
                start..self.pos,
            );
        }
        let text: String = self.src[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        self.push(Tok::Number(text), start);
        Ok(())
    }

    fn operator(&mut self) -> Result<(), FilterError> {
        let start = self.pos;
        while self.peek().is_some_and(|c| OP_CHARS.contains(c)) {
            self.bump();
        }
        let mut text = &self.src[start..self.pos];
        // A boolean operator directly followed by negations (`&&!x`, `||!x`,
        // `!!x`): split off the boolean operator; the `!`s lex next.
        if op_from(text).is_none() {
            for prefix in ["&&", "||", "!"] {
                let rest = &text[prefix.len().min(text.len())..];
                if text.starts_with(prefix) && !rest.is_empty() && rest.chars().all(|c| c == '!') {
                    self.pos = start + prefix.len();
                    text = prefix;
                    break;
                }
            }
        }
        match op_from(text) {
            Some(op) => {
                self.push(Tok::Op(op), start);
                Ok(())
            }
            None => {
                let hint = match text {
                    "=>" => "; did you mean '>='?",
                    "=<" => "; did you mean '<='?",
                    "=!" | "!==" => "; did you mean '!='?",
                    "===" => "; did you mean '='?",
                    "&" => "; did you mean '&&' (or 'and')?",
                    "|" => "; did you mean '||' (or 'or')?",
                    "^" => "; did you mean '^=' (starts with)?",
                    "$" => "; did you mean '$=' (ends with)?",
                    "=~" | "~=" => "; use '~' (contains) or 'matches' (regex)",
                    "!=~" | "!~=" => "; did you mean '!~' (does not contain)?",
                    _ => "",
                };
                self.err(format!("unknown operator '{text}'{hint}"), start..self.pos)
            }
        }
    }
}

fn op_from(text: &str) -> Option<Op> {
    Some(match text {
        "=" | "==" => Op::Eq,
        "!=" | "<>" => Op::Ne,
        "<" => Op::Lt,
        "<=" => Op::Le,
        ">" => Op::Gt,
        ">=" => Op::Ge,
        "~" => Op::Tilde,
        "!~" => Op::NotTilde,
        "^=" => Op::StartsWith,
        "$=" => Op::EndsWith,
        "&&" => Op::AndAnd,
        "||" => Op::OrOr,
        "!" => Op::Bang,
        _ => return None,
    })
}
