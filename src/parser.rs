use crate::diagnostic::{Diagnostic, Phase, Span};
use crate::lexer::{lex, Token, TokenKind as T};
use crate::syntax::*;

pub fn parse(source: &str) -> Result<Program, Vec<Diagnostic>> {
    let tokens = lex(source)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        diagnostics: vec![],
    };
    let program = p.program();
    if p.diagnostics.is_empty() {
        program.ok_or_else(|| vec![p.expected("program")])
    } else {
        Err(p.diagnostics)
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    diagnostics: Vec<Diagnostic>,
}

impl Parser {
    fn program(&mut self) -> Option<Program> {
        self.separators();
        let start = self.current().span.start;
        let mut statements = vec![];
        while !self.at(&T::Return) && !self.at(&T::Eof) {
            if let Some(s) = self.let_stmt() {
                statements.push(s);
            } else {
                self.recover();
            }
            self.require_separator();
            self.separators();
        }
        if !self.take(&T::Return) {
            self.diagnostics
                .push(self.expected("a final `return` statement"));
            return None;
        }
        let result = self.expr(0)?;
        self.separators();
        if !self.at(&T::Eof) {
            self.diagnostics.push(
                Diagnostic::error(
                    "RL1012",
                    Phase::Parse,
                    self.current().span,
                    "early or duplicate return",
                    "a block has exactly one final return",
                )
                .with_fix(
                    self.current().span,
                    "",
                    "remove statements after return",
                ),
            );
        }
        Some(Program {
            span: Span::new(start, result.span.end),
            statements,
            result,
        })
    }
    fn let_stmt(&mut self) -> Option<Stmt> {
        let (name, start) = match self.current().kind.clone() {
            T::Ident(s) => {
                let sp = self.bump().span;
                (s, sp)
            }
            T::If | T::For | T::Boundary => {
                self.diagnostics.push(Diagnostic::error(
                    "RL1014",
                    Phase::Parse,
                    self.current().span,
                    "control structures are expressions",
                    "bind the expression to a name and return that name",
                ));
                return None;
            }
            _ => {
                self.diagnostics
                    .push(self.expected("a binding such as `name = expression`"));
                return None;
            }
        };
        if !self.take(&T::Equal) {
            self.diagnostics
                .push(self.expected("`=` after the binding name"));
            return None;
        }
        let value = self.expr(0)?;
        Some(Stmt {
            span: start.join(value.span),
            name,
            value,
        })
    }
    fn block(&mut self) -> Option<Block> {
        let start = self.expect_take(&T::LBrace, "`{`")?.span;
        self.separators();
        let mut statements = vec![];
        while !self.at(&T::Return) && !self.at(&T::RBrace) && !self.at(&T::Eof) {
            if let Some(s) = self.let_stmt() {
                statements.push(s);
            } else {
                self.recover();
            }
            self.require_separator();
            self.separators();
        }
        if !self.take(&T::Return) {
            let d = Diagnostic::error(
                "RL1017",
                Phase::Parse,
                self.current().span,
                "a block must return a value",
                "write `return expression` as the final statement",
            )
            .with_fix(
                Span::new(self.current().span.start, self.current().span.start),
                "return ",
                "insert `return`",
            );
            self.diagnostics.push(d);
            return None;
        }
        let result = Box::new(self.expr(0)?);
        self.separators();
        let end = self.expect_take(&T::RBrace, "`}` after block return")?.span;
        Some(Block {
            statements,
            result,
            span: start.join(end),
        })
    }
    fn expr(&mut self, min_bp: u8) -> Option<Expr> {
        let mut lhs = self.prefix()?;
        loop {
            if min_bp == 0 && self.take(&T::If) {
                let condition = self.expr(1)?;
                if !self.take(&T::Else) {
                    self.diagnostics
                        .push(self.expected("`else` in conditional expression"));
                    return None;
                }
                let else_expr = self.expr(0)?;
                let span = lhs.span.join(else_expr.span);
                lhs = Expr {
                    span,
                    kind: ExprKind::Conditional {
                        then_expr: Box::new(lhs),
                        condition: Box::new(condition),
                        else_expr: Box::new(else_expr),
                    },
                };
                continue;
            }
            let Some((l, r, op)) = self.infix() else {
                break;
            };
            if l < min_bp {
                break;
            }
            let op_span = self.bump().span;
            let rhs = self.expr(r)?;
            if is_chain_op(op)
                && matches!(lhs.kind, ExprKind::Binary { op: old, .. } if is_chain_op(old))
            {
                self.diagnostics.push(Diagnostic::error(
                    "RL2306",
                    Phase::Analyze,
                    op_span,
                    "chained comparisons are not supported",
                    "join explicit comparisons with `and`, for example `a < b and b < c`",
                ));
            }
            let span = lhs.span.join(rhs.span);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    op,
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                },
            };
        }
        Some(lhs)
    }
    fn prefix(&mut self) -> Option<Expr> {
        let tok = self.bump().clone();
        let mut e = match tok.kind {
            T::Null => Expr {
                span: tok.span,
                kind: ExprKind::Null,
            },
            T::True => Expr {
                span: tok.span,
                kind: ExprKind::Boolean(true),
            },
            T::False => Expr {
                span: tok.span,
                kind: ExprKind::Boolean(false),
            },
            T::Integer(v) => Expr {
                span: tok.span,
                kind: ExprKind::Integer(v),
            },
            T::Number(v) => Expr {
                span: tok.span,
                kind: ExprKind::Number(v),
            },
            T::String(v) => Expr {
                span: tok.span,
                kind: ExprKind::String(v),
            },
            T::Ident(v) => Expr {
                span: tok.span,
                kind: ExprKind::Name(v),
            },
            T::Minus | T::Not => {
                let op = if matches!(tok.kind, T::Minus) {
                    UnaryOp::Negate
                } else {
                    UnaryOp::Not
                };
                let value = self.expr(13)?;
                Expr {
                    span: tok.span.join(value.span),
                    kind: ExprKind::Unary {
                        op,
                        value: Box::new(value),
                    },
                }
            }
            T::LParen => {
                let e = self.expr(0)?;
                self.expect_take(&T::RParen, "`)`")?;
                e
            }
            T::LBracket => self.list(tok.span)?,
            T::LBrace => self.object(tok.span)?,
            T::For => self.for_expr(tok.span)?,
            T::Boundary => self.boundary(tok.span)?,
            _ => {
                self.pos -= 1;
                self.diagnostics.push(self.expected("an expression"));
                return None;
            }
        };
        loop {
            if self.take(&T::Dot) {
                let (field, sp) = self.field_name()?;
                let span = e.span.join(sp);
                e = Expr {
                    span,
                    kind: ExprKind::Member {
                        target: Box::new(e),
                        field,
                    },
                };
            } else if self.take(&T::LBracket) {
                let index = self.expr(0)?;
                let end = self.expect_take(&T::RBracket, "`]`")?.span;
                let span = e.span.join(end);
                e = Expr {
                    span,
                    kind: ExprKind::Index {
                        target: Box::new(e),
                        index: Box::new(index),
                    },
                };
            } else if self.take(&T::LParen) {
                let mut arguments = vec![];
                if !self.at(&T::RParen) {
                    loop {
                        arguments.push(self.expr(0)?);
                        if !self.take(&T::Comma) {
                            break;
                        }
                        if self.at(&T::RParen) {
                            break;
                        }
                    }
                }
                let end = self.expect_take(&T::RParen, "`)` after arguments")?.span;
                let span = e.span.join(end);
                e = Expr {
                    span,
                    kind: ExprKind::Call {
                        callee: Box::new(e),
                        arguments,
                    },
                };
            } else {
                break;
            }
        }
        Some(e)
    }
    fn list(&mut self, start: Span) -> Option<Expr> {
        let mut values = vec![];
        self.separators();
        if !self.at(&T::RBracket) {
            loop {
                values.push(self.expr(0)?);
                self.separators();
                if !self.take(&T::Comma) {
                    break;
                }
                self.separators();
                if self.at(&T::RBracket) {
                    break;
                }
            }
        }
        let end = self.expect_take(&T::RBracket, "`]`")?.span;
        Some(Expr {
            span: start.join(end),
            kind: ExprKind::List(values),
        })
    }
    fn object(&mut self, start: Span) -> Option<Expr> {
        let mut values = vec![];
        self.separators();
        if !self.at(&T::RBrace) {
            loop {
                let (key, key_span, shorthand) = match self.current().kind.clone() {
                    T::String(s) => {
                        let sp = self.bump().span;
                        (s, sp, false)
                    }
                    T::Ident(s) => {
                        let sp = self.bump().span;
                        let shorthand = !self.at(&T::Colon);
                        (s, sp, shorthand)
                    }
                    _ if is_field_token(&self.current().kind) => {
                        let (s, sp) = self.field_name()?;
                        (s, sp, false)
                    }
                    _ => {
                        self.diagnostics
                            .push(self.expected("an object property name"));
                        return None;
                    }
                };
                let value = if shorthand {
                    Expr {
                        span: key_span,
                        kind: ExprKind::Name(key.clone()),
                    }
                } else {
                    self.expect_take(&T::Colon, "`:` after property name")?;
                    self.expr(0)?
                };
                if values.iter().any(|(k, _)| k == &key) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            "RL2203",
                            Phase::Analyze,
                            key_span,
                            "duplicate object property",
                            format!("property `{key}` is already defined"),
                        )
                        .with_fix(
                            key_span.join(value.span),
                            "",
                            "remove the duplicate property",
                        ),
                    );
                }
                values.push((key, value));
                self.separators();
                if !self.take(&T::Comma) {
                    break;
                }
                self.separators();
                if self.at(&T::RBrace) {
                    break;
                }
            }
        }
        let end = self.expect_take(&T::RBrace, "`}`")?.span;
        Some(Expr {
            span: start.join(end),
            kind: ExprKind::Object(values),
        })
    }
    fn for_expr(&mut self, start: Span) -> Option<Expr> {
        let binding = match self.bump().kind.clone() {
            T::Ident(s) => s,
            _ => {
                self.pos -= 1;
                self.diagnostics.push(self.expected("loop binding name"));
                return None;
            }
        };
        self.expect_take(&T::In, "`in`")?;
        let collection = Box::new(self.expr(0)?);
        let limit = if self.take(&T::Limit) {
            match self.bump().kind.clone() {
                T::Integer(s) => s.parse().ok(),
                _ => {
                    self.diagnostics
                        .push(self.expected("positive integer loop limit"));
                    None
                }
            }
        } else {
            None
        };
        let body = self.block()?;
        Some(Expr {
            span: start.join(body.span),
            kind: ExprKind::For {
                binding,
                collection,
                limit,
                body,
            },
        })
    }
    fn boundary(&mut self, start: Span) -> Option<Expr> {
        let retries = if self.take(&T::Retry) {
            match self.bump().kind.clone() {
                T::Integer(s) => s.parse().unwrap_or(u32::MAX),
                _ => {
                    self.diagnostics.push(self.expected("integer retry count"));
                    0
                }
            }
        } else {
            0
        };
        let body = self.block()?;
        if self.at(&T::Newline) || self.at(&T::Semicolon) {
            self.diagnostics.push(Diagnostic::error(
                "RL1019",
                Phase::Parse,
                self.current().span,
                "`catch` must follow the boundary on the same line",
                "write `} catch err {` without a statement break",
            ));
            self.separators();
        }
        self.expect_take(&T::Catch, "`catch` after boundary body")?;
        let error_binding = match self.bump().kind.clone() {
            T::Ident(s) => s,
            _ => {
                self.diagnostics.push(self.expected("catch error binding"));
                return None;
            }
        };
        let catch = self.block()?;
        Some(Expr {
            span: start.join(catch.span),
            kind: ExprKind::Boundary {
                retries,
                body,
                error_binding,
                catch,
            },
        })
    }
    fn infix(&self) -> Option<(u8, u8, BinaryOp)> {
        Some(match self.current().kind {
            T::Or => (1, 2, BinaryOp::Or),
            T::And => (3, 4, BinaryOp::And),
            T::EqualEqual => (5, 6, BinaryOp::Equal),
            T::BangEqual => (5, 6, BinaryOp::NotEqual),
            T::Less => (7, 8, BinaryOp::Less),
            T::LessEqual => (7, 8, BinaryOp::LessEqual),
            T::Greater => (7, 8, BinaryOp::Greater),
            T::GreaterEqual => (7, 8, BinaryOp::GreaterEqual),
            T::In => (7, 8, BinaryOp::In),
            T::Plus => (9, 10, BinaryOp::Add),
            T::Minus => (9, 10, BinaryOp::Subtract),
            T::Star => (11, 12, BinaryOp::Multiply),
            T::Slash => (11, 12, BinaryOp::Divide),
            T::Percent => (11, 12, BinaryOp::Remainder),
            _ => return None,
        })
    }
    fn field_name(&mut self) -> Option<(String, Span)> {
        let t = self.bump().clone();
        let s = match t.kind {
            T::Ident(s) => s,
            T::Return => "return".into(),
            T::For => "for".into(),
            T::In => "in".into(),
            T::Limit => "limit".into(),
            T::Boundary => "boundary".into(),
            T::Retry => "retry".into(),
            T::Catch => "catch".into(),
            T::If => "if".into(),
            T::Else => "else".into(),
            T::And => "and".into(),
            T::Or => "or".into(),
            T::Not => "not".into(),
            T::Null => "null".into(),
            T::True => "true".into(),
            T::False => "false".into(),
            _ => {
                self.pos -= 1;
                self.diagnostics.push(self.expected("field name"));
                return None;
            }
        };
        Some((s, t.span))
    }
    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn bump(&mut self) -> &Token {
        let i = self.pos;
        if !self.at(&T::Eof) {
            self.pos += 1;
        }
        &self.tokens[i]
    }
    fn at(&self, kind: &T) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(kind)
    }
    fn take(&mut self, kind: &T) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect_take(&mut self, kind: &T, what: &str) -> Option<Token> {
        if self.at(kind) {
            Some(self.bump().clone())
        } else {
            self.diagnostics.push(self.expected(what));
            None
        }
    }
    fn expected(&self, what: &str) -> Diagnostic {
        Diagnostic::error(
            "RL1008",
            Phase::Parse,
            self.current().span,
            "unexpected token",
            format!("expected {what}"),
        )
        .with_fix(
            Span::new(self.current().span.start, self.current().span.start),
            "",
            format!("insert {what}"),
        )
    }
    fn separators(&mut self) {
        while self.at(&T::Newline) || self.at(&T::Semicolon) {
            self.bump();
        }
    }
    fn require_separator(&mut self) {
        if !(self.at(&T::Newline)
            || self.at(&T::Semicolon)
            || self.at(&T::RBrace)
            || self.at(&T::Eof))
        {
            self.diagnostics.push(self.expected("a newline or `;`"));
        }
    }
    fn recover(&mut self) {
        while !matches!(
            self.current().kind,
            T::Newline | T::Semicolon | T::RBrace | T::Eof
        ) {
            self.bump();
        }
    }
}

fn is_chain_op(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
            | BinaryOp::In
    )
}
fn is_field_token(t: &T) -> bool {
    matches!(
        t,
        T::Return
            | T::For
            | T::In
            | T::Limit
            | T::Boundary
            | T::Retry
            | T::Catch
            | T::If
            | T::Else
            | T::And
            | T::Or
            | T::Not
            | T::Null
            | T::True
            | T::False
    )
}
