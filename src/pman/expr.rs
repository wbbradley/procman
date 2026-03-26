use anyhow::{Result, bail};

use crate::pman::{
    ast::{BinOp, Expr},
    token::{Span, Token, TokenKind},
};

pub struct ExprParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    path: &'a str,
}

impl<'a> ExprParser<'a> {
    pub fn new(tokens: &'a [Token], path: &'a str) -> Self {
        Self {
            tokens,
            pos: 0,
            path,
        }
    }

    pub fn parse(&mut self) -> Result<Expr> {
        self.parse_or()
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek(&self) -> Option<&TokenKind> {
        self.tokens.get(self.pos).map(|t| &t.kind)
    }

    fn span(&self) -> Span {
        if self.pos < self.tokens.len() {
            self.tokens[self.pos].span
        } else if !self.tokens.is_empty() {
            self.tokens[self.tokens.len() - 1].span
        } else {
            Span {
                start: 0,
                end: 0,
                line: 1,
                col: 1,
            }
        }
    }

    fn advance(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        self.pos += 1;
        tok
    }

    fn expect(&mut self, expected: &TokenKind) -> Result<&Token> {
        if self.at_end() {
            bail!(
                "{}",
                self.span().fmt_error(
                    self.path,
                    &format!("expected {:?}, got end of input", expected)
                )
            );
        }
        let tok = &self.tokens[self.pos];
        if &tok.kind != expected {
            bail!(
                "{}",
                tok.span.fmt_error(
                    self.path,
                    &format!("expected {:?}, got {:?}", expected, tok.kind)
                )
            );
        }
        self.pos += 1;
        Ok(tok)
    }

    // Precedence level 1: ||
    fn parse_or(&mut self) -> Result<Expr> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&TokenKind::Or) {
            let op_span = self.advance().span;
            let right = self.parse_and()?;
            let span = merge_spans(expr_span(&left), expr_span(&right));
            let _ = op_span;
            left = Expr::BinOp(Box::new(left), BinOp::Or, Box::new(right), span);
        }
        Ok(left)
    }

    // Precedence level 2: &&
    fn parse_and(&mut self) -> Result<Expr> {
        let mut left = self.parse_comparison()?;
        while self.peek() == Some(&TokenKind::And) {
            self.advance();
            let right = self.parse_comparison()?;
            let span = merge_spans(expr_span(&left), expr_span(&right));
            left = Expr::BinOp(Box::new(left), BinOp::And, Box::new(right), span);
        }
        Ok(left)
    }

    // Precedence level 3: ==, !=, <, >, <=, >=
    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(TokenKind::Eq) => BinOp::Eq,
                Some(TokenKind::Ne) => BinOp::Ne,
                Some(TokenKind::Lt) => BinOp::Lt,
                Some(TokenKind::Gt) => BinOp::Gt,
                Some(TokenKind::Le) => BinOp::Le,
                Some(TokenKind::Ge) => BinOp::Ge,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            let span = merge_spans(expr_span(&left), expr_span(&right));
            left = Expr::BinOp(Box::new(left), op, Box::new(right), span);
        }
        Ok(left)
    }

    // Precedence level 4: ! (prefix unary not)
    fn parse_unary(&mut self) -> Result<Expr> {
        if self.peek() == Some(&TokenKind::Not) {
            let start_span = self.advance().span;
            let operand = self.parse_unary()?;
            let span = merge_spans(start_span, expr_span(&operand));
            return Ok(Expr::UnaryNot(Box::new(operand), span));
        }
        self.parse_atom()
    }

    // Precedence level 5: atoms
    fn parse_atom(&mut self) -> Result<Expr> {
        if self.at_end() {
            bail!(
                "{}",
                self.span()
                    .fmt_error(self.path, "expected expression, got end of input")
            );
        }

        match self.peek().unwrap().clone() {
            TokenKind::String(s) => {
                let span = self.advance().span;
                Ok(Expr::StringLit(s, span))
            }
            TokenKind::Number(n) => {
                let span = self.advance().span;
                Ok(Expr::NumberLit(n, span))
            }
            TokenKind::Duration(d) => {
                let span = self.advance().span;
                Ok(Expr::DurationLit(d, span))
            }
            TokenKind::True => {
                let span = self.advance().span;
                Ok(Expr::BoolLit(true, span))
            }
            TokenKind::False => {
                let span = self.advance().span;
                Ok(Expr::BoolLit(false, span))
            }
            TokenKind::None => {
                let span = self.advance().span;
                Ok(Expr::NoneLit(span))
            }
            // args.name
            TokenKind::Args => {
                let start_span = self.advance().span;
                self.expect(&TokenKind::Dot)?;
                if self.at_end() {
                    bail!(
                        "{}",
                        start_span.fmt_error(self.path, "expected identifier after 'args.'")
                    );
                }
                match self.peek().unwrap().clone() {
                    TokenKind::Ident(name) => {
                        let end_span = self.advance().span;
                        let span = merge_spans(start_span, end_span);
                        Ok(Expr::ArgsRef(name, span))
                    }
                    other => {
                        bail!(
                            "{}",
                            self.span().fmt_error(
                                self.path,
                                &format!("expected identifier after 'args.', got {:?}", other)
                            )
                        );
                    }
                }
            }
            // @job.KEY
            TokenKind::At => {
                let start_span = self.advance().span;
                if self.at_end() {
                    bail!(
                        "{}",
                        start_span.fmt_error(self.path, "expected identifier after '@'")
                    );
                }
                let job_name = match self.peek().unwrap().clone() {
                    TokenKind::Ident(name) => {
                        self.advance();
                        name
                    }
                    other => {
                        bail!(
                            "{}",
                            self.span().fmt_error(
                                self.path,
                                &format!("expected job name after '@', got {:?}", other)
                            )
                        );
                    }
                };
                self.expect(&TokenKind::Dot)?;
                if self.at_end() {
                    bail!(
                        "{}",
                        self.span()
                            .fmt_error(self.path, &format!("expected key after '@{job_name}.'"))
                    );
                }
                match self.peek().unwrap().clone() {
                    TokenKind::Ident(key) => {
                        let end_span = self.advance().span;
                        let span = merge_spans(start_span, end_span);
                        Ok(Expr::JobOutputRef(job_name, key, span))
                    }
                    other => {
                        bail!(
                            "{}",
                            self.span().fmt_error(
                                self.path,
                                &format!("expected key after '@{job_name}.', got {:?}", other)
                            )
                        );
                    }
                }
            }
            // parenthesized sub-expression
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse()?;
                self.expect(&TokenKind::RParen)?;
                Ok(inner)
            }
            // local variable
            TokenKind::Ident(name) => {
                let span = self.advance().span;
                Ok(Expr::LocalVar(name, span))
            }
            other => {
                bail!(
                    "{}",
                    self.span().fmt_error(
                        self.path,
                        &format!("unexpected token in expression: {:?}", other)
                    )
                );
            }
        }
    }
}

#[cfg(test)]
/// Convenience wrapper: parse a complete token slice as a single expression.
pub fn parse_expr(tokens: &[Token], path: &str) -> Result<Expr> {
    let mut parser = ExprParser::new(tokens, path);
    let expr = parser.parse()?;
    if !parser.at_end() {
        bail!(
            "{}",
            parser.span().fmt_error(
                path,
                &format!(
                    "unexpected token after expression: {:?}",
                    parser.peek().unwrap()
                )
            )
        );
    }
    Ok(expr)
}

fn expr_span(expr: &Expr) -> Span {
    match expr {
        Expr::StringLit(_, s)
        | Expr::NumberLit(_, s)
        | Expr::BoolLit(_, s)
        | Expr::DurationLit(_, s)
        | Expr::NoneLit(s)
        | Expr::ArgsRef(_, s)
        | Expr::JobOutputRef(_, _, s)
        | Expr::LocalVar(_, s)
        | Expr::BinOp(_, _, _, s)
        | Expr::UnaryNot(_, s) => *s,
    }
}

fn merge_spans(a: Span, b: Span) -> Span {
    Span {
        start: a.start.min(b.start),
        end: a.end.max(b.end),
        line: a.line,
        col: a.col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pman::lexer::lex;

    fn parse(input: &str) -> Expr {
        let tokens = lex(input, 1, 1, "test.pman").unwrap();
        parse_expr(&tokens, "test.pman").unwrap()
    }

    #[test]
    fn string_literal() {
        let expr = parse(r#""hello""#);
        assert!(matches!(expr, Expr::StringLit(s, _) if s == "hello"));
    }

    #[test]
    fn number_literal() {
        let expr = parse("42");
        assert!(matches!(expr, Expr::NumberLit(n, _) if n == 42.0));
    }

    #[test]
    fn bool_literal() {
        let expr = parse("true");
        assert!(matches!(expr, Expr::BoolLit(true, _)));
        let expr = parse("false");
        assert!(matches!(expr, Expr::BoolLit(false, _)));
    }

    #[test]
    fn duration_literal() {
        let expr = parse("5s");
        assert!(matches!(expr, Expr::DurationLit(d, _) if d == 5.0));
    }

    #[test]
    fn args_ref() {
        let expr = parse("args.port");
        assert!(matches!(expr, Expr::ArgsRef(name, _) if name == "port"));
    }

    #[test]
    fn job_output_ref() {
        let expr = parse("@migrate.exit_code");
        assert!(
            matches!(expr, Expr::JobOutputRef(job, key, _) if job == "migrate" && key == "exit_code")
        );
    }

    #[test]
    fn comparison() {
        let expr = parse("args.port == 8080");
        assert!(matches!(expr, Expr::BinOp(_, BinOp::Eq, _, _)));
    }

    #[test]
    fn logical_and() {
        let expr = parse("true && false");
        assert!(matches!(expr, Expr::BinOp(_, BinOp::And, _, _)));
    }

    #[test]
    fn unary_not() {
        let expr = parse("!true");
        assert!(
            matches!(expr, Expr::UnaryNot(inner, _) if matches!(*inner, Expr::BoolLit(true, _)))
        );
    }

    #[test]
    fn grouped_expression() {
        let expr = parse("(true || false) && true");
        // outer should be And
        assert!(matches!(expr, Expr::BinOp(_, BinOp::And, _, _)));
        if let Expr::BinOp(left, BinOp::And, _, _) = &expr {
            // inner (left) should be Or
            assert!(matches!(left.as_ref(), Expr::BinOp(_, BinOp::Or, _, _)));
        }
    }

    #[test]
    fn local_var() {
        let expr = parse("myvar");
        assert!(matches!(expr, Expr::LocalVar(name, _) if name == "myvar"));
    }

    #[test]
    fn none_literal() {
        let expr = parse("none");
        assert!(matches!(expr, Expr::NoneLit(_)));
    }
}
