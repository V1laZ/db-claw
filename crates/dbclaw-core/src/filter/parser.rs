//! Recursive-descent parser: tokens → [`Expr`].

use super::lexer::{Op, Tok, Token, lex};
use super::{CompareOp, Expr, Field, FieldPath, FilterError, Literal, is_keyword, normalize_date};
use std::ops::Range;

/// Parse a filter. Empty / whitespace-only input yields `Ok(None)`.
pub fn parse(input: &str) -> Result<Option<Expr>, FilterError> {
    let tokens = lex(input)?;
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut p = Parser {
        src: input,
        toks: tokens,
        pos: 0,
    };
    let expr = p.parse_or()?;
    if let Some(t) = p.peek() {
        let msg = match &t.tok {
            Tok::RParen => "unmatched ')'".to_string(),
            _ => format!("expected 'and' or 'or' before '{}'", p.text(&t.span)),
        };
        return Err(FilterError::new(msg, t.span.clone()));
    }
    Ok(Some(expr))
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
}

type PResult<T> = Result<T, FilterError>;

fn kw(tok: &Tok, word: &str) -> bool {
    matches!(tok, Tok::Ident(s) if s.eq_ignore_ascii_case(word))
}

impl Parser<'_> {
    fn text(&self, span: &Range<usize>) -> &str {
        &self.src[span.clone()]
    }

    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    fn peek_tok(&self) -> Option<&Tok> {
        self.peek().map(|t| &t.tok)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eof_span(&self) -> Range<usize> {
        self.src.len()..self.src.len()
    }

    /// Span of the previously consumed token's end (for "after X" errors).
    fn prev_end(&self) -> usize {
        self.pos
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(0, |t| t.span.end)
    }

    fn prev_text(&self) -> &str {
        match self.pos.checked_sub(1).and_then(|i| self.toks.get(i)) {
            Some(t) => self.text(&t.span),
            None => "",
        }
    }

    /// Span of the next token, or the end of input.
    fn here(&self) -> Range<usize> {
        self.peek()
            .map_or_else(|| self.eof_span(), |t| t.span.clone())
    }

    fn eat_kw(&mut self, word: &str) -> bool {
        if self.peek_tok().is_some_and(|t| kw(t, word)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_op(&mut self, op: Op) -> bool {
        if self.peek_tok() == Some(&Tok::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    // ---- boolean structure --------------------------------------------------

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut items = vec![self.parse_and()?];
        while self.eat_kw("or") || self.eat_op(Op::OrOr) {
            items.push(self.parse_and()?);
        }
        Ok(flatten(items, true))
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut items = vec![self.parse_unary()?];
        while self.eat_kw("and") || self.eat_op(Op::AndAnd) {
            items.push(self.parse_unary()?);
        }
        Ok(flatten(items, false))
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.eat_kw("not") || self.eat_op(Op::Bang) {
            return Ok(Expr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let Some(tok) = self.peek().cloned() else {
            let msg = if self.pos == 0 {
                "expected a condition".to_string()
            } else {
                format!("expected a condition after '{}'", self.prev_text())
            };
            return Err(FilterError::new(msg, self.eof_span()));
        };
        match &tok.tok {
            Tok::LParen => {
                self.pos += 1;
                let inner = self.parse_or()?;
                match self.peek_tok() {
                    Some(Tok::RParen) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    None => Err(FilterError::new(
                        "missing ')' to close '('",
                        tok.span.start..self.src.len(),
                    )),
                    Some(_) => {
                        let span = self.here();
                        Err(FilterError::new(
                            format!("expected ')' or 'and'/'or' before '{}'", self.text(&span)),
                            span,
                        ))
                    }
                }
            }
            Tok::Ident(word) if is_keyword(word) => Err(FilterError::new(
                format!(
                    "expected a field name, found keyword '{word}'; write `{word}` to use it as a field name"
                ),
                tok.span.clone(),
            )),
            Tok::Ident(_) | Tok::QuotedIdent(_) => {
                let field = self.parse_field()?;
                self.parse_predicate(field)
            }
            Tok::Str(s) => Err(FilterError::new(
                format!(
                    "expected a field name; quote field names with backticks: `{}`",
                    s.replace('`', "``")
                ),
                tok.span.clone(),
            )),
            Tok::RParen if self.pos > 0 => Err(FilterError::new(
                format!("expected a condition after '{}'", self.prev_text()),
                tok.span.clone(),
            )),
            _ => Err(FilterError::new(
                format!("expected a field name, found '{}'", self.text(&tok.span)),
                tok.span.clone(),
            )),
        }
    }

    fn parse_field(&mut self) -> PResult<Field> {
        let first = self.next().expect("caller checked");
        let start = first.span.start;
        let mut end = first.span.end;
        let mut segs = vec![match first.tok {
            Tok::Ident(s) | Tok::QuotedIdent(s) => s,
            _ => unreachable!("caller checked"),
        }];
        while self.peek_tok() == Some(&Tok::Dot) {
            self.pos += 1;
            match self.next() {
                Some(Token {
                    tok: Tok::Ident(s) | Tok::QuotedIdent(s),
                    span,
                }) => {
                    segs.push(s);
                    end = span.end;
                }
                other => {
                    let span = other.map_or_else(|| self.eof_span(), |t| t.span);
                    return Err(FilterError::new("expected a field name after '.'", span));
                }
            }
        }
        Ok(Field {
            path: FieldPath(segs),
            span: start..end,
        })
    }

    // ---- conditions ---------------------------------------------------------

    fn parse_predicate(&mut self, field: Field) -> PResult<Expr> {
        let start = field.span.start;
        let fname = self.text(&field.span).to_string();
        let Some(tok) = self.next() else {
            return Err(FilterError::new(
                format!("expected an operator after '{fname}' (e.g. =, !=, ~, in, is null)"),
                self.eof_span(),
            ));
        };
        let op = match &tok.tok {
            Tok::Op(op) => match op {
                Op::Eq => CompareOp::Eq,
                Op::Ne => CompareOp::Ne,
                Op::Lt => CompareOp::Lt,
                Op::Le => CompareOp::Le,
                Op::Gt => CompareOp::Gt,
                Op::Ge => CompareOp::Ge,
                Op::Tilde => CompareOp::Contains,
                Op::NotTilde => CompareOp::NotContains,
                Op::StartsWith => CompareOp::StartsWith,
                Op::EndsWith => CompareOp::EndsWith,
                Op::AndAnd | Op::OrOr | Op::Bang => {
                    return Err(missing_op(&fname, &tok.span, self.text(&tok.span)));
                }
            },
            t if kw(t, "in") => return self.parse_in(field, false, start),
            t if kw(t, "between") => return self.parse_between(field, start),
            t if kw(t, "exists") => {
                return Ok(Expr::Exists {
                    field,
                    negated: false,
                    span: start..tok.span.end,
                });
            }
            t if kw(t, "is") => {
                let negated = self.eat_kw("not");
                if !self.eat_kw("null") {
                    let what = if negated { "'is not'" } else { "'is'" };
                    return Err(FilterError::new(
                        format!("expected 'null' after {what}"),
                        self.here(),
                    ));
                }
                return Ok(Expr::IsNull {
                    field,
                    negated,
                    span: start..self.prev_end(),
                });
            }
            t if kw(t, "not") => {
                let Some(next) = self.next() else {
                    return Err(FilterError::new(
                        "expected 'in', 'like', 'ilike', 'matches', 'between' or 'exists' after 'not'",
                        self.eof_span(),
                    ));
                };
                let t = &next.tok;
                if kw(t, "in") {
                    return self.parse_in(field, true, start);
                }
                if kw(t, "exists") {
                    return Ok(Expr::Exists {
                        field,
                        negated: true,
                        span: start..next.span.end,
                    });
                }
                let inner = if kw(t, "between") {
                    self.parse_between(field, start)?
                } else if kw(t, "like") || kw(t, "ilike") || kw(t, "matches") {
                    let op = keyword_op(t);
                    self.parse_compare(field, op, &next.span, start)?
                } else if matches!(t, Tok::Op(Op::Eq)) {
                    return Err(FilterError::new(
                        "unexpected '=' after 'not'; did you mean '!='?",
                        next.span,
                    ));
                } else {
                    return Err(FilterError::new(
                        "expected 'in', 'like', 'ilike', 'matches', 'between' or 'exists' after 'not'",
                        next.span,
                    ));
                };
                return Ok(Expr::Not(Box::new(inner)));
            }
            t if kw(t, "like") || kw(t, "ilike") || kw(t, "matches") => keyword_op(t),
            Tok::Ident(word) => {
                let hint = match word.to_ascii_lowercase().as_str() {
                    "contains" | "has" | "includes" => Some("~"),
                    "startswith" | "starts" | "starts_with" | "prefix" => Some("^="),
                    "endswith" | "ends" | "ends_with" | "suffix" => Some("$="),
                    "eq" | "equals" | "is_eq" => Some("="),
                    "ne" | "neq" => Some("!="),
                    "gt" => Some(">"),
                    "gte" | "ge" => Some(">="),
                    "lt" => Some("<"),
                    "lte" | "le" => Some("<="),
                    "regex" | "match" | "rlike" | "regexp" => Some("matches"),
                    "nin" => Some("not in"),
                    "exist" => Some("exists"),
                    _ => None,
                };
                return Err(match hint {
                    Some(h) => FilterError::new(
                        format!("unknown operator '{word}'; did you mean '{h}'?"),
                        tok.span.clone(),
                    ),
                    None => missing_op(&fname, &tok.span, word),
                });
            }
            _ => return Err(missing_op(&fname, &tok.span, self.text(&tok.span))),
        };
        self.parse_compare(field, op, &tok.span, start)
    }

    fn parse_compare(
        &mut self,
        field: Field,
        op: CompareOp,
        op_span: &Range<usize>,
        start: usize,
    ) -> PResult<Expr> {
        let op_src = self.text(op_span).to_string();
        let (value, vspan) = self.parse_value(&op_src)?;
        let span = start..vspan.end;
        let compare = |op, value| Expr::Compare {
            field: field.clone(),
            op,
            value,
            span: span.clone(),
        };
        match (&value, op) {
            (Literal::Null, CompareOp::Eq) => Ok(Expr::IsNull {
                field,
                negated: false,
                span,
            }),
            (Literal::Null, CompareOp::Ne) => Ok(Expr::IsNull {
                field,
                negated: true,
                span,
            }),
            (Literal::Null, _) => Err(FilterError::new(
                format!("null can't be used with '{op_src}'; use 'is null' or 'is not null'"),
                vspan,
            )),
            (Literal::Regex { .. }, CompareOp::Eq | CompareOp::Contains | CompareOp::Matches) => {
                Ok(compare(CompareOp::Matches, value))
            }
            (Literal::Regex { .. }, CompareOp::Ne | CompareOp::NotContains) => {
                Ok(Expr::Not(Box::new(compare(CompareOp::Matches, value))))
            }
            (Literal::Regex { .. }, _) => Err(FilterError::new(
                format!("a regex can't be used with '{op_src}'; use '=', '!=' or 'matches'"),
                vspan,
            )),
            (Literal::String(_), _) => Ok(compare(op, value)),
            (
                _,
                CompareOp::Contains
                | CompareOp::NotContains
                | CompareOp::StartsWith
                | CompareOp::EndsWith
                | CompareOp::Like
                | CompareOp::ILike
                | CompareOp::Matches,
            ) => Err(FilterError::new(
                format!(
                    "'{op_src}' needs text in quotes, e.g. \"{}\"",
                    self.text(&vspan).trim_matches('"')
                ),
                vspan,
            )),
            _ => Ok(compare(op, value)),
        }
    }

    fn parse_in(&mut self, field: Field, negated: bool, start: usize) -> PResult<Expr> {
        let open = self.next();
        let close = match open.as_ref().map(|t| &t.tok) {
            Some(Tok::LBracket) => Tok::RBracket,
            Some(Tok::LParen) => Tok::RParen,
            _ => {
                let span = open.map_or_else(|| self.eof_span(), |t| t.span);
                return Err(FilterError::new(
                    "expected a list after 'in', e.g. in [\"a\", \"b\"]",
                    span,
                ));
            }
        };
        let open = open.expect("matched above");
        let close_ch = if close == Tok::RBracket { "]" } else { ")" };
        let mut values = Vec::new();
        loop {
            if self.peek_tok() == Some(&close) {
                self.pos += 1;
                break;
            }
            let after = self.prev_text().to_string();
            let (v, _) = self.parse_value(&after)?;
            values.push(v);
            match self.peek_tok() {
                Some(Tok::Comma) => self.pos += 1,
                Some(t) if *t == close => {}
                Some(_) => {
                    return Err(FilterError::new(
                        format!("expected ',' or '{close_ch}' in the list"),
                        self.here(),
                    ));
                }
                None => {
                    return Err(FilterError::new(
                        format!("missing '{close_ch}' to close the list"),
                        open.span.start..self.src.len(),
                    ));
                }
            }
        }
        Ok(Expr::In {
            field,
            values,
            negated,
            span: start..self.prev_end(),
        })
    }

    fn parse_between(&mut self, field: Field, start: usize) -> PResult<Expr> {
        let (low, lspan) = self.parse_value("between")?;
        if !self.eat_kw("and") {
            return Err(FilterError::new(
                "expected 'and' in 'between … and …'",
                self.here(),
            ));
        }
        let (high, hspan) = self.parse_value("and")?;
        for (v, span) in [(&low, lspan), (&high, hspan)] {
            if matches!(v, Literal::Null | Literal::Regex { .. }) {
                return Err(FilterError::new(
                    "'between' needs two values like numbers, text or dates",
                    span,
                ));
            }
        }
        Ok(Expr::Between {
            field,
            low,
            high,
            span: start..self.prev_end(),
        })
    }

    // ---- values -------------------------------------------------------------

    fn parse_value(&mut self, after: &str) -> PResult<(Literal, Range<usize>)> {
        let Some(tok) = self.next() else {
            return Err(FilterError::new(
                format!("expected a value after '{after}'"),
                self.eof_span(),
            ));
        };
        let span = tok.span.clone();
        let lit = match tok.tok {
            Tok::Str(s) => Literal::String(s),
            Tok::Number(text) => parse_number(&text, &span)?,
            Tok::Regex { pattern, flags } => Literal::Regex { pattern, flags },
            Tok::Ident(w) if w.eq_ignore_ascii_case("true") => Literal::Bool(true),
            Tok::Ident(w) if w.eq_ignore_ascii_case("false") => Literal::Bool(false),
            Tok::Ident(w) if w.eq_ignore_ascii_case("null") => Literal::Null,
            Tok::Ident(w) if self.peek_tok() == Some(&Tok::LParen) => {
                return self.parse_function(&w, span);
            }
            Tok::Ident(w) if is_keyword(&w) => {
                return Err(FilterError::new(
                    format!("expected a value after '{after}', found '{w}'"),
                    span,
                ));
            }
            Tok::Ident(w) => {
                return Err(FilterError::new(
                    format!("expected a value; did you mean \"{w}\"?"),
                    span,
                ));
            }
            Tok::QuotedIdent(w) => {
                return Err(FilterError::new(
                    format!(
                        "expected a value; backticks are for field names, use \"{w}\" for text"
                    ),
                    span,
                ));
            }
            Tok::LBracket => {
                return Err(FilterError::new(
                    "a list is only allowed after 'in' / 'not in'",
                    span,
                ));
            }
            _ => {
                return Err(FilterError::new(
                    format!("expected a value after '{after}'"),
                    span,
                ));
            }
        };
        Ok((lit, span))
    }

    fn parse_function(
        &mut self,
        name: &str,
        name_span: Range<usize>,
    ) -> PResult<(Literal, Range<usize>)> {
        let lname = name.to_ascii_lowercase();
        let (is_date, example) = match lname.as_str() {
            "date" | "datetime" | "isodate" | "timestamp" => (true, "date(\"2024-01-01\")"),
            "oid" | "objectid" => (false, "oid(\"65a1f0c2e4b0a1b2c3d4e5f6\")"),
            _ => {
                return Err(FilterError::new(
                    format!(
                        "unknown function '{name}'; available: date(\"2024-01-01\"), oid(\"…\")"
                    ),
                    name_span,
                ));
            }
        };
        self.pos += 1; // '('
        let arg = match self.next() {
            Some(Token {
                tok: Tok::Str(s),
                span,
            }) => (s, span),
            other => {
                let span = other.map_or_else(|| self.eof_span(), |t| t.span);
                return Err(FilterError::new(
                    format!("{lname}() expects a quoted string, e.g. {example}"),
                    span,
                ));
            }
        };
        match self.next() {
            Some(Token {
                tok: Tok::RParen, ..
            }) => {}
            other => {
                let span = other.map_or_else(|| self.eof_span(), |t| t.span);
                return Err(FilterError::new(
                    format!("missing ')' after {lname}(…"),
                    span,
                ));
            }
        }
        let span = name_span.start..self.prev_end();
        let (text, arg_span) = arg;
        let lit = if is_date {
            if normalize_date(&text).is_none() {
                return Err(FilterError::new(
                    format!(
                        "invalid date \"{text}\"; use YYYY-MM-DD or an ISO timestamp like 2024-01-01T10:00:00Z"
                    ),
                    arg_span,
                ));
            }
            Literal::Date(text)
        } else {
            if text.len() != 24 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(FilterError::new(
                    "an ObjectId must be exactly 24 hex characters",
                    arg_span,
                ));
            }
            Literal::ObjectId(text.to_ascii_lowercase())
        };
        Ok((lit, span))
    }
}

fn keyword_op(t: &Tok) -> CompareOp {
    if kw(t, "like") {
        CompareOp::Like
    } else if kw(t, "ilike") {
        CompareOp::ILike
    } else {
        CompareOp::Matches
    }
}

fn missing_op(field: &str, span: &Range<usize>, found: &str) -> FilterError {
    FilterError::new(
        format!(
            "expected an operator after '{field}' (e.g. =, !=, ~, in, is null), found '{found}'"
        ),
        span.clone(),
    )
}

fn parse_number(text: &str, span: &Range<usize>) -> PResult<Literal> {
    if text.contains(['.', 'e', 'E']) {
        match text.parse::<f64>() {
            Ok(x) if x.is_finite() => Ok(Literal::Float(x)),
            _ => Err(FilterError::new("number is out of range", span.clone())),
        }
    } else {
        text.parse::<i64>().map(Literal::Int).map_err(|_| {
            FilterError::new(
                "number is too large (max 9223372036854775807)",
                span.clone(),
            )
        })
    }
}

fn flatten(items: Vec<Expr>, or: bool) -> Expr {
    if items.len() == 1 {
        return items.into_iter().next().expect("one item");
    }
    let mut out = Vec::with_capacity(items.len());
    for e in items {
        match e {
            Expr::Or(inner) if or => out.extend(inner),
            Expr::And(inner) if !or => out.extend(inner),
            e => out.push(e),
        }
    }
    if or { Expr::Or(out) } else { Expr::And(out) }
}
