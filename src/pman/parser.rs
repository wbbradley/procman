use anyhow::{Result, bail};

use crate::pman::{
    ast::{self, Expr},
    expr::ExprParser,
    lexer,
    token::{Span, Token, TokenKind},
};

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
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
                "{}: expected {:?}, got end of input",
                fmt_span(self.span()),
                expected
            );
        }
        let tok = &self.tokens[self.pos];
        if &tok.kind != expected {
            bail!(
                "{}: expected {:?}, got {:?}",
                fmt_span(tok.span),
                expected,
                tok.kind
            );
        }
        self.pos += 1;
        Ok(&self.tokens[self.pos - 1])
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek() == Some(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_expr(&mut self) -> Result<Expr> {
        let mut ep = ExprParser::new(&self.tokens[self.pos..]);
        let expr = ep.parse()?;
        self.pos += ep.pos();
        Ok(expr)
    }

    fn expect_string_lit(&mut self) -> Result<ast::StringLit> {
        if self.at_end() {
            bail!(
                "{}: expected string literal, got end of input",
                fmt_span(self.span())
            );
        }
        let tok = &self.tokens[self.pos];
        match &tok.kind {
            TokenKind::String(s) => {
                let lit = ast::StringLit {
                    value: s.clone(),
                    span: tok.span,
                };
                self.pos += 1;
                Ok(lit)
            }
            other => bail!(
                "{}: expected string literal, got {:?}",
                fmt_span(tok.span),
                other
            ),
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span)> {
        if self.at_end() {
            bail!(
                "{}: expected identifier, got end of input",
                fmt_span(self.span())
            );
        }
        let tok = &self.tokens[self.pos];
        match &tok.kind {
            TokenKind::Ident(name) => {
                let result = (name.clone(), tok.span);
                self.pos += 1;
                Ok(result)
            }
            other => bail!(
                "{}: expected identifier, got {:?}",
                fmt_span(tok.span),
                other
            ),
        }
    }

    fn parse_file(&mut self) -> Result<ast::File> {
        let mut file = ast::File {
            config: None,
            jobs: Vec::new(),
            events: Vec::new(),
        };

        while !self.at_end() {
            match self.peek() {
                Some(TokenKind::Config) => {
                    if file.config.is_some() {
                        bail!("{}: duplicate config block", fmt_span(self.span()));
                    }
                    file.config = Some(self.parse_config_block()?);
                }
                Some(TokenKind::Job) => {
                    // Task 6
                    bail!("{}: job parsing not yet implemented", fmt_span(self.span()));
                }
                Some(TokenKind::Event) => {
                    // Task 6
                    bail!(
                        "{}: event parsing not yet implemented",
                        fmt_span(self.span())
                    );
                }
                Some(other) => {
                    bail!(
                        "{}: expected 'config', 'job', or 'event', got {:?}",
                        fmt_span(self.span()),
                        other
                    );
                }
                None => break,
            }
        }

        Ok(file)
    }

    fn parse_config_block(&mut self) -> Result<ast::ConfigBlock> {
        let start_span = self.expect(&TokenKind::Config)?.span;
        self.expect(&TokenKind::LBrace)?;

        let mut config = ast::ConfigBlock {
            logs: None,
            env: Vec::new(),
            args: Vec::new(),
            span: start_span,
        };

        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}: unterminated config block", fmt_span(start_span));
            }
            match self.peek() {
                Some(TokenKind::Ident(name)) if name == "logs" => {
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    config.logs = Some(self.expect_string_lit()?);
                }
                Some(TokenKind::Arg) => {
                    config.args.push(self.parse_arg_def()?);
                }
                Some(TokenKind::Env) => {
                    self.parse_env_entry_or_block(&mut config.env)?;
                }
                Some(other) => {
                    bail!(
                        "{}: unexpected token in config block: {:?}",
                        fmt_span(self.span()),
                        other
                    );
                }
                None => unreachable!(),
            }
        }

        config.span = merge_spans(start_span, self.tokens[self.pos - 1].span);
        Ok(config)
    }

    fn parse_arg_def(&mut self) -> Result<ast::ArgDef> {
        let start_span = self.expect(&TokenKind::Arg)?.span;
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace)?;

        let mut arg = ast::ArgDef {
            name,
            arg_type: None,
            default: None,
            short: None,
            description: None,
            span: start_span,
        };

        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}: unterminated arg block", fmt_span(start_span));
            }
            let (field_name, field_span) = self.expect_ident()?;
            self.expect(&TokenKind::Assign)?;
            match field_name.as_str() {
                "type" => {
                    let (type_name, type_span) = self.expect_ident()?;
                    arg.arg_type = Some(match type_name.as_str() {
                        "string" => ast::ArgType::String,
                        "bool" => ast::ArgType::Bool,
                        _ => bail!(
                            "{}: unknown arg type '{}', expected 'string' or 'bool'",
                            fmt_span(type_span),
                            type_name
                        ),
                    });
                }
                "default" => {
                    arg.default = Some(self.parse_expr()?);
                }
                "short" => {
                    arg.short = Some(self.expect_string_lit()?);
                }
                "description" => {
                    arg.description = Some(self.expect_string_lit()?);
                }
                _ => bail!(
                    "{}: unknown arg field '{}'",
                    fmt_span(field_span),
                    field_name
                ),
            }
        }

        arg.span = merge_spans(start_span, self.tokens[self.pos - 1].span);
        Ok(arg)
    }

    fn parse_env_entry_or_block(&mut self, env: &mut Vec<ast::EnvBinding>) -> Result<()> {
        self.expect(&TokenKind::Env)?;

        if self.eat(&TokenKind::LBrace) {
            // env { KEY = expr ... }
            while !self.eat(&TokenKind::RBrace) {
                if self.at_end() {
                    bail!("{}: unterminated env block", fmt_span(self.span()));
                }
                env.push(self.parse_env_binding()?);
            }
        } else {
            // env KEY = expr
            env.push(self.parse_env_binding()?);
        }

        Ok(())
    }

    fn parse_env_binding(&mut self) -> Result<ast::EnvBinding> {
        let (key, key_span) = self.expect_ident()?;
        self.expect(&TokenKind::Assign)?;
        let value = self.parse_expr()?;
        let value_span = expr_span(&value);
        Ok(ast::EnvBinding {
            key,
            value,
            span: merge_spans(key_span, value_span),
        })
    }
}

pub fn parse(input: &str) -> Result<ast::File> {
    let tokens = lexer::lex(input, 1, 1)?;
    let mut parser = Parser::new(tokens);
    parser.parse_file()
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

fn fmt_span(span: Span) -> String {
    format!("{}:{}", span.line, span.col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_config() {
        let file = parse("config {}").unwrap();
        let config = file.config.unwrap();
        assert!(config.logs.is_none());
        assert!(config.args.is_empty());
        assert!(config.env.is_empty());
    }

    #[test]
    fn parse_config_logs() {
        let file = parse(r#"config { logs = "./my-logs" }"#).unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.logs.unwrap().value, "./my-logs");
    }

    #[test]
    fn parse_config_arg() {
        let input = r#"
            config {
                arg port {
                    type = string
                    default = "8080"
                    short = "-p"
                    description = "Port to listen on"
                }
            }
        "#;
        let file = parse(input).unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.args.len(), 1);
        let arg = &config.args[0];
        assert_eq!(arg.name, "port");
        assert_eq!(arg.arg_type, Some(ast::ArgType::String));
        assert!(matches!(&arg.default, Some(Expr::StringLit(s, _)) if s == "8080"));
        assert_eq!(arg.short.as_ref().unwrap().value, "-p");
        assert_eq!(arg.description.as_ref().unwrap().value, "Port to listen on");
    }

    #[test]
    fn parse_config_env_block() {
        let input = r#"
            config {
                env {
                    NODE_ENV = "production"
                    PORT = args.port
                }
            }
        "#;
        let file = parse(input).unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.env.len(), 2);
        assert_eq!(config.env[0].key, "NODE_ENV");
        assert!(matches!(&config.env[0].value, Expr::StringLit(s, _) if s == "production"));
        assert_eq!(config.env[1].key, "PORT");
        assert!(matches!(&config.env[1].value, Expr::ArgsRef(name, _) if name == "port"));
    }

    #[test]
    fn parse_config_bool_arg() {
        let input = r#"
            config {
                arg verbose {
                    type = bool
                    default = false
                    short = "-v"
                    description = "Enable verbose output"
                }
            }
        "#;
        let file = parse(input).unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.args.len(), 1);
        let arg = &config.args[0];
        assert_eq!(arg.name, "verbose");
        assert_eq!(arg.arg_type, Some(ast::ArgType::Bool));
        assert!(matches!(&arg.default, Some(Expr::BoolLit(false, _))));
        assert_eq!(arg.short.as_ref().unwrap().value, "-v");
    }
}
