use anyhow::{Result, bail};

use crate::pman::{
    ast::{self, Expr},
    expr::ExprParser,
    lexer,
    token::{Span, Token, TokenKind},
};

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    path: &'a str,
}

impl<'a> Parser<'a> {
    fn new(tokens: Vec<Token>, path: &'a str) -> Self {
        Self {
            tokens,
            pos: 0,
            path,
        }
    }

    fn fmt_error(&self, span: Span, msg: &str) -> String {
        span.fmt_error(self.path, msg)
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
                self.fmt_error(
                    self.span(),
                    &format!("expected {:?}, got end of input", expected)
                )
            );
        }
        let tok = &self.tokens[self.pos];
        if &tok.kind != expected {
            bail!(
                "{}",
                self.fmt_error(
                    tok.span,
                    &format!("expected {:?}, got {:?}", expected, tok.kind)
                )
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
        let mut ep = ExprParser::new(&self.tokens[self.pos..], self.path);
        let expr = ep.parse()?;
        self.pos += ep.pos();
        Ok(expr)
    }

    fn expect_string_lit(&mut self) -> Result<ast::StringLit> {
        if self.at_end() {
            bail!(
                "{}",
                self.fmt_error(self.span(), "expected string literal, got end of input")
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
                "{}",
                self.fmt_error(
                    tok.span,
                    &format!("expected string literal, got {:?}", other)
                )
            ),
        }
    }

    fn expect_ident(&mut self) -> Result<(String, Span)> {
        if self.at_end() {
            bail!(
                "{}",
                self.fmt_error(self.span(), "expected identifier, got end of input")
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
                "{}",
                self.fmt_error(tok.span, &format!("expected identifier, got {:?}", other))
            ),
        }
    }

    fn parse_file(&mut self) -> Result<ast::File> {
        let mut file = ast::File {
            imports: Vec::new(),
            config: None,
            args: Vec::new(),
            env: Vec::new(),
            jobs: Vec::new(),
            services: Vec::new(),
            events: Vec::new(),
            tasks: Vec::new(),
        };

        while !self.at_end() {
            match self.peek() {
                Some(TokenKind::Import) => {
                    file.imports.push(self.parse_import_def()?);
                }
                Some(TokenKind::Config) => {
                    if file.config.is_some() {
                        bail!("{}", self.fmt_error(self.span(), "duplicate config block"));
                    }
                    file.config = Some(self.parse_config_block()?);
                }
                Some(TokenKind::Arg) => {
                    file.args.push(self.parse_arg_def()?);
                }
                Some(TokenKind::Job) => {
                    file.jobs.push(self.parse_job_def()?);
                }
                Some(TokenKind::Service) => {
                    file.services.push(self.parse_service_def()?);
                }
                Some(TokenKind::Env) => {
                    self.parse_env_entry_or_block(&mut file.env)?;
                }
                Some(TokenKind::Event) => {
                    file.events.push(self.parse_event_def()?);
                }
                Some(TokenKind::Task) => {
                    file.tasks.push(self.parse_task_def()?);
                }
                Some(other) => {
                    bail!(
                        "{}",
                        self.fmt_error(
                            self.span(),
                            &format!(
                                "expected 'import', 'config', 'arg', 'env', 'job', 'service', 'event', or 'task', got {:?}",
                                other
                            )
                        )
                    );
                }
                None => break,
            }
        }

        Ok(file)
    }

    fn parse_import_def(&mut self) -> Result<ast::ImportDef> {
        let start_span = self.expect(&TokenKind::Import)?.span;
        let path = self.expect_string_lit()?;
        let alias = if self.eat(&TokenKind::As) {
            let (name, _) = self.expect_ident()?;
            name
        } else {
            derive_import_alias(&path.value, start_span, self.path)?
        };
        let bindings = if self.eat(&TokenKind::LBrace) {
            self.parse_import_bindings()?
        } else {
            Vec::new()
        };
        let end_span = self.tokens[self.pos - 1].span;
        Ok(ast::ImportDef {
            path,
            alias,
            bindings,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_import_bindings(&mut self) -> Result<Vec<ast::ImportBinding>> {
        let mut bindings = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!(
                    "{}",
                    self.fmt_error(self.span(), "unterminated import binding block")
                );
            }
            let (name, name_span) = self.expect_ident()?;
            self.expect(&TokenKind::Assign)?;
            let value = self.parse_expr()?;
            let value_span = value.span();
            bindings.push(ast::ImportBinding {
                name,
                value,
                span: merge_spans(name_span, value_span),
            });
        }
        Ok(bindings)
    }

    /// Parse `@name` or `@ns::name`, returning `(namespace, name, span)`.
    fn parse_at_ref(&mut self) -> Result<(Option<String>, String, Span)> {
        let start_span = self.expect(&TokenKind::At)?.span;
        let (first, _) = self.expect_ident()?;
        if self.eat(&TokenKind::ColonColon) {
            let (second, end_span) = self.expect_ident()?;
            Ok((Some(first), second, merge_spans(start_span, end_span)))
        } else {
            let end_span = self.tokens[self.pos - 1].span;
            Ok((None, first, merge_spans(start_span, end_span)))
        }
    }

    fn parse_config_block(&mut self) -> Result<ast::ConfigBlock> {
        let start_span = self.expect(&TokenKind::Config)?.span;
        self.expect(&TokenKind::LBrace)?;

        let mut config = ast::ConfigBlock {
            logs: None,
            log_time: None,
            span: start_span,
        };

        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!(
                    "{}",
                    self.fmt_error(start_span, "unterminated config block")
                );
            }
            match self.peek() {
                Some(TokenKind::Ident(name)) if name == "logs" => {
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    config.logs = Some(self.expect_string_lit()?);
                }
                Some(TokenKind::Ident(name)) if name == "log_time" => {
                    let name_span = self.span();
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    let expr = self.parse_expr()?;
                    match expr {
                        Expr::BoolLit(v, _) => {
                            config.log_time = Some(v);
                        }
                        _ => {
                            bail!(
                                "{}",
                                self.fmt_error(name_span, "log_time must be true or false")
                            );
                        }
                    }
                }
                Some(other) => {
                    bail!(
                        "{}",
                        self.fmt_error(
                            self.span(),
                            &format!("unexpected token in config block: {:?}", other)
                        )
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
                bail!("{}", self.fmt_error(start_span, "unterminated arg block"));
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
                            "{}",
                            self.fmt_error(
                                type_span,
                                &format!(
                                    "unknown arg type '{}', expected 'string' or 'bool'",
                                    type_name
                                )
                            )
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
                    "{}",
                    self.fmt_error(field_span, &format!("unknown arg field '{}'", field_name))
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
                    bail!("{}", self.fmt_error(self.span(), "unterminated env block"));
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
        let value_span = value.span();
        Ok(ast::EnvBinding {
            key,
            value,
            span: merge_spans(key_span, value_span),
        })
    }

    fn parse_job_def(&mut self) -> Result<ast::JobDef> {
        let start_span = self.expect(&TokenKind::Job)?.span;
        let (name, _) = self.expect_ident()?;
        let condition = if self.eat(&TokenKind::If) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace)?;
        let body = self.parse_job_body()?;
        let end_span = self.expect(&TokenKind::RBrace)?.span;
        Ok(ast::JobDef {
            name,
            condition,
            body,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_service_def(&mut self) -> Result<ast::ServiceDef> {
        let start_span = self.expect(&TokenKind::Service)?.span;
        let (name, _) = self.expect_ident()?;
        let condition = if self.eat(&TokenKind::If) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace)?;
        let body = self.parse_job_body()?;
        let end_span = self.expect(&TokenKind::RBrace)?.span;
        Ok(ast::ServiceDef {
            name,
            condition,
            body,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_event_def(&mut self) -> Result<ast::EventDef> {
        let start_span = self.expect(&TokenKind::Event)?.span;
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace)?;
        let body = self.parse_job_body()?;
        let end_span = self.expect(&TokenKind::RBrace)?.span;
        Ok(ast::EventDef {
            name,
            body,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_task_def(&mut self) -> Result<ast::TaskDef> {
        let start_span = self.expect(&TokenKind::Task)?.span;
        let (name, _) = self.expect_ident()?;
        let condition = if self.eat(&TokenKind::If) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(&TokenKind::LBrace)?;
        let body = self.parse_job_body()?;
        let end_span = self.expect(&TokenKind::RBrace)?.span;
        Ok(ast::TaskDef {
            name,
            condition,
            body,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_job_body(&mut self) -> Result<ast::JobBody> {
        let mut env = Vec::new();
        let mut wait = None;
        let mut watches = Vec::new();
        let mut run_section: Option<ast::RunSection> = None;

        while self.peek() != Some(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}", self.fmt_error(self.span(), "unterminated job body"));
            }
            match self.peek() {
                Some(TokenKind::Env) => {
                    self.parse_env_entry_or_block(&mut env)?;
                }
                Some(TokenKind::Wait) => {
                    wait = Some(self.parse_wait_block()?);
                }
                Some(TokenKind::Watch) => {
                    watches.push(self.parse_watch_def()?);
                }
                Some(TokenKind::Run) => {
                    self.advance();
                    let shell = self.parse_shell_block()?;
                    run_section = Some(ast::RunSection::Direct(shell));
                }
                Some(TokenKind::For) => {
                    let for_loop = self.parse_for_loop()?;
                    run_section = Some(ast::RunSection::ForLoop(Box::new(for_loop)));
                }
                Some(other) => {
                    bail!(
                        "{}",
                        self.fmt_error(
                            self.span(),
                            &format!("unexpected token in job body: {:?}", other)
                        )
                    );
                }
                None => unreachable!(),
            }
        }

        let run_section = run_section.ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                self.fmt_error(self.span(), "missing 'run' or 'for' in job body")
            )
        })?;

        Ok(ast::JobBody {
            env,
            wait,
            watches,
            run_section,
        })
    }

    fn parse_shell_block(&mut self) -> Result<ast::ShellBlock> {
        match self.peek() {
            Some(TokenKind::String(_)) => {
                let tok = self.advance();
                let span = tok.span;
                let s = match &tok.kind {
                    TokenKind::String(s) => s.clone(),
                    _ => unreachable!(),
                };
                Ok(ast::ShellBlock::Inline(ast::StringLit { value: s, span }))
            }
            Some(TokenKind::FencedString(_)) => {
                let tok = self.advance();
                let span = tok.span;
                let s = match &tok.kind {
                    TokenKind::FencedString(s) => s.clone(),
                    _ => unreachable!(),
                };
                Ok(ast::ShellBlock::Fenced(s, span))
            }
            _ => {
                bail!(
                    "{}",
                    self.fmt_error(self.span(), "expected string or fenced string after 'run'")
                )
            }
        }
    }

    fn parse_wait_block(&mut self) -> Result<ast::WaitBlock> {
        let start_span = self.expect(&TokenKind::Wait)?.span;
        self.expect(&TokenKind::LBrace)?;
        let mut conditions = Vec::new();
        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}", self.fmt_error(start_span, "unterminated wait block"));
            }
            conditions.push(self.parse_wait_condition()?);
        }
        let end_span = self.tokens[self.pos - 1].span;
        Ok(ast::WaitBlock {
            conditions,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_wait_condition(&mut self) -> Result<ast::WaitCondition> {
        let start_span = self.span();
        let negated = self.eat(&TokenKind::Not);

        match self.peek() {
            Some(TokenKind::After) => {
                self.advance();
                let (namespace, job, _) = self.parse_at_ref()?;
                let options = self.parse_condition_options()?;
                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::After { namespace, job },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(TokenKind::Http) => {
                self.advance();
                let url = self.expect_string_lit()?;
                let options = self.parse_condition_options()?;
                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::Http { url },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(TokenKind::Connect) => {
                self.advance();
                let address = self.expect_string_lit()?;
                let options = self.parse_condition_options()?;
                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::Connect { address },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(TokenKind::Exists) => {
                self.advance();
                let path = self.expect_string_lit()?;
                let options = self.parse_condition_options()?;
                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::Exists { path },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(TokenKind::Running) => {
                self.advance();
                let pattern = self.expect_string_lit()?;
                let options = self.parse_condition_options()?;
                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::Running { pattern },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(TokenKind::Contains) => {
                self.advance();
                let path = self.expect_string_lit()?;
                self.expect(&TokenKind::LBrace)?;

                let mut format = None;
                let mut key = None;
                let mut var = None;
                let mut options = ast::ConditionOptions::default();

                while !self.eat(&TokenKind::RBrace) {
                    if self.at_end() {
                        bail!(
                            "{}",
                            self.fmt_error(start_span, "unterminated contains block")
                        );
                    }
                    let (field_name, field_span) = self.expect_ident()?;
                    self.expect(&TokenKind::Assign)?;
                    match field_name.as_str() {
                        "format" => {
                            let lit = self.expect_string_lit()?;
                            format = Some(lit.value);
                        }
                        "key" => {
                            key = Some(self.expect_string_lit()?);
                        }
                        "var" => {
                            let (var_name, _) = self.expect_ident()?;
                            var = Some(var_name);
                        }
                        "timeout" => options.timeout = Some(self.parse_expr()?),
                        "poll" => options.poll = Some(self.parse_expr()?),
                        "retry" => options.retry = Some(self.parse_expr()?),
                        "status" => options.status = Some(self.parse_expr()?),
                        _ => bail!(
                            "{}",
                            self.fmt_error(
                                field_span,
                                &format!("unknown field '{}' in contains block", field_name)
                            )
                        ),
                    }
                }

                let format = format.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.fmt_error(start_span, "missing 'format' in contains block")
                    )
                })?;
                let key = key.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.fmt_error(start_span, "missing 'key' in contains block")
                    )
                })?;

                let end_span = self.tokens[self.pos - 1].span;
                Ok(ast::WaitCondition {
                    negated,
                    kind: ast::ConditionKind::Contains {
                        path,
                        format,
                        key,
                        var,
                    },
                    options,
                    span: merge_spans(start_span, end_span),
                })
            }
            Some(other) => {
                bail!(
                    "{}",
                    self.fmt_error(
                        self.span(),
                        &format!("expected wait condition keyword, got {:?}", other)
                    )
                )
            }
            None => {
                bail!(
                    "{}",
                    self.fmt_error(self.span(), "expected wait condition, got end of input")
                )
            }
        }
    }

    fn parse_condition_options(&mut self) -> Result<ast::ConditionOptions> {
        let mut options = ast::ConditionOptions::default();
        if self.peek() != Some(&TokenKind::LBrace) {
            return Ok(options);
        }
        let start_span = self.span();
        self.advance(); // consume LBrace
        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!(
                    "{}",
                    self.fmt_error(start_span, "unterminated condition options block")
                );
            }
            let (field_name, field_span) = self.expect_ident()?;
            self.expect(&TokenKind::Assign)?;
            match field_name.as_str() {
                "status" => options.status = Some(self.parse_expr()?),
                "timeout" => options.timeout = Some(self.parse_expr()?),
                "poll" => options.poll = Some(self.parse_expr()?),
                "retry" => options.retry = Some(self.parse_expr()?),
                _ => bail!(
                    "{}",
                    self.fmt_error(
                        field_span,
                        &format!("unknown condition option '{}'", field_name)
                    )
                ),
            }
        }
        Ok(options)
    }

    fn parse_watch_def(&mut self) -> Result<ast::WatchDef> {
        let start_span = self.expect(&TokenKind::Watch)?.span;
        let (name, _) = self.expect_ident()?;
        self.expect(&TokenKind::LBrace)?;

        let condition = self.parse_wait_condition()?;

        let mut initial_delay = None;
        let mut poll = None;
        let mut threshold = None;
        let mut on_fail = None;

        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}", self.fmt_error(start_span, "unterminated watch block"));
            }
            match self.peek() {
                Some(TokenKind::Ident(name)) if name == "initial_delay" => {
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    initial_delay = Some(self.parse_expr()?);
                }
                Some(TokenKind::Ident(name)) if name == "poll" => {
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    poll = Some(self.parse_expr()?);
                }
                Some(TokenKind::Ident(name)) if name == "threshold" => {
                    self.advance();
                    self.expect(&TokenKind::Assign)?;
                    threshold = Some(self.parse_expr()?);
                }
                Some(TokenKind::OnFail) => {
                    on_fail = Some(self.parse_on_fail()?);
                }
                Some(other) => {
                    bail!(
                        "{}",
                        self.fmt_error(
                            self.span(),
                            &format!("unexpected token in watch block: {:?}", other)
                        )
                    )
                }
                None => unreachable!(),
            }
        }

        let end_span = self.tokens[self.pos - 1].span;
        Ok(ast::WatchDef {
            name,
            condition,
            initial_delay,
            poll,
            threshold,
            on_fail,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_on_fail(&mut self) -> Result<ast::OnFailAction> {
        self.expect(&TokenKind::OnFail)?;
        match self.peek() {
            Some(TokenKind::Ident(name)) if name == "shutdown" => {
                self.advance();
                Ok(ast::OnFailAction::Shutdown)
            }
            Some(TokenKind::Ident(name)) if name == "debug" => {
                self.advance();
                Ok(ast::OnFailAction::Debug)
            }
            Some(TokenKind::Ident(name)) if name == "log" => {
                self.advance();
                Ok(ast::OnFailAction::Log)
            }
            Some(TokenKind::Spawn) => {
                self.advance();
                let (namespace, name, _) = self.parse_at_ref()?;
                Ok(ast::OnFailAction::Spawn(namespace, name))
            }
            _ => {
                bail!(
                    "{}",
                    self.fmt_error(
                        self.span(),
                        "expected 'shutdown', 'debug', 'log', or 'spawn' after 'on_fail'"
                    )
                )
            }
        }
    }

    fn parse_for_loop(&mut self) -> Result<ast::ForLoop> {
        let start_span = self.expect(&TokenKind::For)?.span;
        let (var, _) = self.expect_ident()?;
        self.expect(&TokenKind::In)?;
        let iterable = self.parse_iterable()?;
        self.expect(&TokenKind::LBrace)?;

        let mut env = Vec::new();
        let mut run = None;

        while !self.eat(&TokenKind::RBrace) {
            if self.at_end() {
                bail!("{}", self.fmt_error(start_span, "unterminated for block"));
            }
            match self.peek() {
                Some(TokenKind::Env) => {
                    self.parse_env_entry_or_block(&mut env)?;
                }
                Some(TokenKind::Run) => {
                    self.advance();
                    run = Some(self.parse_shell_block()?);
                }
                Some(other) => {
                    bail!(
                        "{}",
                        self.fmt_error(
                            self.span(),
                            &format!("unexpected token in for block: {:?}", other)
                        )
                    )
                }
                None => unreachable!(),
            }
        }

        let run = run.ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                self.fmt_error(self.span(), "missing 'run' in for block")
            )
        })?;
        let end_span = self.tokens[self.pos - 1].span;

        Ok(ast::ForLoop {
            var,
            iterable,
            env,
            run,
            span: merge_spans(start_span, end_span),
        })
    }

    fn parse_iterable(&mut self) -> Result<ast::Iterable> {
        match self.peek() {
            Some(TokenKind::Glob) => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let pattern = self.expect_string_lit()?;
                self.expect(&TokenKind::RParen)?;
                Ok(ast::Iterable::Glob(pattern))
            }
            Some(TokenKind::LBracket) => {
                self.advance();
                let mut items = Vec::new();
                while !self.eat(&TokenKind::RBracket) {
                    if self.at_end() {
                        bail!(
                            "{}",
                            self.fmt_error(self.span(), "unterminated array literal")
                        );
                    }
                    if !items.is_empty() {
                        self.expect(&TokenKind::Comma)?;
                    }
                    items.push(self.parse_expr()?);
                }
                Ok(ast::Iterable::Array(items))
            }
            Some(TokenKind::Number(_)) => {
                let start = self.parse_expr()?;
                if self.eat(&TokenKind::DotDotEq) {
                    let end = self.parse_expr()?;
                    Ok(ast::Iterable::RangeInclusive(start, end))
                } else if self.eat(&TokenKind::DotDot) {
                    let end = self.parse_expr()?;
                    Ok(ast::Iterable::RangeExclusive(start, end))
                } else {
                    bail!(
                        "{}",
                        self.fmt_error(self.span(), "expected '..' or '..=' after range start")
                    );
                }
            }
            _ => {
                bail!(
                    "{}",
                    self.fmt_error(self.span(), "expected 'glob', '[', or number for iterable")
                )
            }
        }
    }
}

pub fn parse(input: &str, path: &str) -> Result<ast::File> {
    let tokens = lexer::lex(input, 1, 1, path)?;
    let mut parser = Parser::new(tokens, path);
    parser.parse_file()
}

fn derive_import_alias(path_str: &str, span: Span, file_path: &str) -> Result<String> {
    // Extract basename from path.
    let basename = path_str.rsplit('/').next().unwrap_or(path_str);
    // Strip .pman extension.
    let stem = basename.strip_suffix(".pman").unwrap_or(basename);
    // Validate it's a legal identifier.
    let bytes = stem.as_bytes();
    if bytes.is_empty()
        || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        || !bytes[1..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!(
            "{}",
            span.fmt_error(
                file_path,
                &format!("cannot derive alias from '{}'; use 'as <alias>'", path_str)
            )
        );
    }
    Ok(stem.to_string())
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

    #[test]
    fn parse_empty_config() {
        let file = parse("config {}", "test.pman").unwrap();
        let config = file.config.unwrap();
        assert!(config.logs.is_none());
    }

    #[test]
    fn parse_config_logs() {
        let file = parse(r#"config { logs = "./my-logs" }"#, "test.pman").unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.logs.unwrap().value, "./my-logs");
    }

    #[test]
    fn parse_top_level_arg() {
        let input = r#"
            arg port {
                type = string
                default = "8080"
                short = "-p"
                description = "Port to listen on"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.args.len(), 1);
        let arg = &file.args[0];
        assert_eq!(arg.name, "port");
        assert_eq!(arg.arg_type, Some(ast::ArgType::String));
        assert!(matches!(&arg.default, Some(Expr::StringLit(s, _)) if s == "8080"));
        assert_eq!(arg.short.as_ref().unwrap().value, "-p");
        assert_eq!(arg.description.as_ref().unwrap().value, "Port to listen on");
    }

    #[test]
    fn parse_top_level_env_block() {
        let input = r#"
            env {
                NODE_ENV = "production"
                PORT = args.port
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.env.len(), 2);
        assert_eq!(file.env[0].key, "NODE_ENV");
        assert!(matches!(&file.env[0].value, Expr::StringLit(s, _) if s == "production"));
        assert_eq!(file.env[1].key, "PORT");
        assert!(matches!(&file.env[1].value, Expr::ArgsRef(name, _) if name == "port"));
    }

    #[test]
    fn parse_top_level_env_single() {
        let input = r#"env NODE_ENV = "production""#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.env.len(), 1);
        assert_eq!(file.env[0].key, "NODE_ENV");
        assert!(matches!(&file.env[0].value, Expr::StringLit(s, _) if s == "production"));
    }

    #[test]
    fn parse_env_rejected_in_config() {
        let input = r#"config { env { NODE_ENV = "production" } }"#;
        let err = parse(input, "test.pman").unwrap_err();
        assert!(
            err.to_string().contains("unexpected token in config block"),
            "got: {}",
            err
        );
    }

    #[test]
    fn parse_config_log_time_true() {
        let file = parse("config { log_time = true }", "test.pman").unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.log_time, Some(true));
    }

    #[test]
    fn parse_config_log_time_false() {
        let file = parse("config { log_time = false }", "test.pman").unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.log_time, Some(false));
    }

    #[test]
    fn parse_config_log_time_default() {
        let file = parse("config {}", "test.pman").unwrap();
        let config = file.config.unwrap();
        assert!(config.log_time.is_none());
    }

    #[test]
    fn parse_config_log_time_with_logs() {
        let file = parse(
            r#"config { logs = "./my-logs" log_time = true }"#,
            "test.pman",
        )
        .unwrap();
        let config = file.config.unwrap();
        assert_eq!(config.logs.unwrap().value, "./my-logs");
        assert_eq!(config.log_time, Some(true));
    }

    // ── Job/event tests ──

    #[test]
    fn parse_simple_job() {
        let file = parse(r#"job web { run "serve" }"#, "test.pman").unwrap();
        assert_eq!(file.jobs.len(), 1);
        let job = &file.jobs[0];
        assert_eq!(job.name, "web");
        assert!(job.condition.is_none());
        assert!(
            matches!(&job.body.run_section, ast::RunSection::Direct(ast::ShellBlock::Inline(s)) if s.value == "serve")
        );
    }

    #[test]
    fn parse_job_with_condition() {
        let file = parse(
            r#"job worker if args.enable_worker { run "worker start" }"#,
            "test.pman",
        )
        .unwrap();
        let job = &file.jobs[0];
        assert_eq!(job.name, "worker");
        assert!(matches!(&job.condition, Some(Expr::ArgsRef(name, _)) if name == "enable_worker"));
    }

    #[test]
    fn parse_service() {
        let file = parse(r#"service web { run "serve" }"#, "test.pman").unwrap();
        assert_eq!(file.services.len(), 1);
        assert_eq!(file.services[0].name, "web");
    }

    #[test]
    fn once_in_job_body_is_error() {
        assert!(parse(r#"job migrate { once = true run "migrate" }"#, "test.pman").is_err());
    }

    #[test]
    fn parse_event() {
        let file = parse(r#"event recovery { run "./recover.sh" }"#, "test.pman").unwrap();
        assert_eq!(file.events.len(), 1);
        assert_eq!(file.events[0].name, "recovery");
        assert!(
            matches!(&file.events[0].body.run_section, ast::RunSection::Direct(ast::ShellBlock::Inline(s)) if s.value == "./recover.sh")
        );
    }

    #[test]
    fn parse_job_with_env() {
        let input = r#"
            job api {
                env PORT = "3000"
                env {
                    HOST = "localhost"
                }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let job = &file.jobs[0];
        assert_eq!(job.body.env.len(), 2);
        assert_eq!(job.body.env[0].key, "PORT");
        assert_eq!(job.body.env[1].key, "HOST");
    }

    #[test]
    fn parse_fenced_run() {
        let input = "job migrate { run \"\"\"\n  ./run-migrations\n\"\"\" }";
        let file = parse(input, "test.pman").unwrap();
        let job = &file.jobs[0];
        assert!(
            matches!(&job.body.run_section, ast::RunSection::Direct(ast::ShellBlock::Fenced(s, _)) if s.contains("run-migrations"))
        );
    }

    // ── Wait tests ──

    #[test]
    fn parse_wait_after() {
        let input = r#"
            job api {
                wait { after @migrate }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let wait = file.jobs[0].body.wait.as_ref().unwrap();
        assert_eq!(wait.conditions.len(), 1);
        assert!(
            matches!(&wait.conditions[0].kind, ast::ConditionKind::After { namespace: None, job } if job == "migrate")
        );
        assert!(!wait.conditions[0].negated);
    }

    #[test]
    fn parse_wait_http_with_options() {
        let input = r#"
            job api {
                wait {
                    http "http://localhost:3000/health" {
                        status = 200
                        timeout = 30s
                        poll = 500ms
                    }
                }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(
            matches!(&cond.kind, ast::ConditionKind::Http { url } if url.value == "http://localhost:3000/health")
        );
        assert!(matches!(&cond.options.status, Some(Expr::NumberLit(n, _)) if *n == 200.0));
        assert!(matches!(&cond.options.timeout, Some(Expr::DurationLit(n, _)) if *n == 30.0));
        assert!(matches!(&cond.options.poll, Some(Expr::DurationLit(n, _)) if *n == 0.5));
    }

    #[test]
    fn parse_wait_negated() {
        let input = r#"
            job checker {
                wait { !connect "127.0.0.1:8080" }
                run "check"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(cond.negated);
        assert!(
            matches!(&cond.kind, ast::ConditionKind::Connect { address } if address.value == "127.0.0.1:8080")
        );
    }

    #[test]
    fn parse_wait_timeout_none() {
        let input = r#"
            job api {
                wait {
                    after @setup {
                        timeout = none
                    }
                }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(matches!(&cond.options.timeout, Some(Expr::NoneLit(_))));
    }

    #[test]
    fn parse_wait_retry_false() {
        let input = r#"
            job api {
                wait {
                    connect "127.0.0.1:5432" {
                        retry = false
                    }
                }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(matches!(&cond.options.retry, Some(Expr::BoolLit(false, _))));
    }

    #[test]
    fn parse_wait_contains_with_var() {
        let input = r#"
            job api {
                wait {
                    contains "/tmp/config.yaml" {
                        format = "yaml"
                        key = "$.database.url"
                        var = database_url
                    }
                }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        match &cond.kind {
            ast::ConditionKind::Contains {
                path,
                format,
                key,
                var,
            } => {
                assert_eq!(path.value, "/tmp/config.yaml");
                assert_eq!(format, "yaml");
                assert_eq!(key.value, "$.database.url");
                assert_eq!(var.as_deref(), Some("database_url"));
            }
            _ => panic!("expected Contains condition"),
        }
    }

    // ── Watch/for tests ──

    #[test]
    fn parse_watch() {
        let input = r#"
            job web {
                run "web-server"
                watch health {
                    http "http://localhost:8080/health" {
                        status = 200
                    }
                    initial_delay = 5s
                    poll = 10s
                    threshold = 3
                    on_fail shutdown
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let job = &file.jobs[0];
        assert_eq!(job.body.watches.len(), 1);
        let w = &job.body.watches[0];
        assert_eq!(w.name, "health");
        assert!(
            matches!(&w.condition.kind, ast::ConditionKind::Http { url } if url.value == "http://localhost:8080/health")
        );
        assert!(matches!(&w.initial_delay, Some(Expr::DurationLit(n, _)) if *n == 5.0));
        assert!(matches!(&w.poll, Some(Expr::DurationLit(n, _)) if *n == 10.0));
        assert!(matches!(&w.threshold, Some(Expr::NumberLit(n, _)) if *n == 3.0));
        assert!(matches!(&w.on_fail, Some(ast::OnFailAction::Shutdown)));
    }

    #[test]
    fn parse_watch_spawn() {
        let input = r#"
            job web {
                run "web-server"
                watch disk {
                    exists "/var/run/healthy"
                    on_fail spawn @recovery
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let w = &file.jobs[0].body.watches[0];
        assert_eq!(w.name, "disk");
        assert!(
            matches!(&w.on_fail, Some(ast::OnFailAction::Spawn(None, name)) if name == "recovery")
        );
    }

    #[test]
    fn parse_for_glob() {
        let input = r#"
            job nodes {
                for config_path in glob("configs/*.yaml") {
                    env NODE_CONFIG = config_path
                    run "start-node"
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        match &file.jobs[0].body.run_section {
            ast::RunSection::ForLoop(fl) => {
                assert_eq!(fl.var, "config_path");
                assert!(
                    matches!(&fl.iterable, ast::Iterable::Glob(s) if s.value == "configs/*.yaml")
                );
                assert_eq!(fl.env.len(), 1);
                assert_eq!(fl.env[0].key, "NODE_CONFIG");
            }
            _ => panic!("expected ForLoop"),
        }
    }

    #[test]
    fn parse_for_array() {
        let input = r#"
            job multi {
                for name in ["a", "b", "c"] {
                    run "start"
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        match &file.jobs[0].body.run_section {
            ast::RunSection::ForLoop(fl) => {
                assert_eq!(fl.var, "name");
                match &fl.iterable {
                    ast::Iterable::Array(items) => assert_eq!(items.len(), 3),
                    _ => panic!("expected Array"),
                }
            }
            _ => panic!("expected ForLoop"),
        }
    }

    #[test]
    fn parse_for_range() {
        let input = r#"
            job shards {
                for i in 0..3 {
                    run "shard"
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        match &file.jobs[0].body.run_section {
            ast::RunSection::ForLoop(fl) => {
                assert_eq!(fl.var, "i");
                assert!(matches!(&fl.iterable, ast::Iterable::RangeExclusive(
                    Expr::NumberLit(start, _),
                    Expr::NumberLit(end, _)
                ) if *start == 0.0 && *end == 3.0));
            }
            _ => panic!("expected ForLoop"),
        }
    }

    #[test]
    fn parse_top_level_bool_arg() {
        let input = r#"
            arg verbose {
                type = bool
                default = false
                short = "-v"
                description = "Enable verbose output"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.args.len(), 1);
        let arg = &file.args[0];
        assert_eq!(arg.name, "verbose");
        assert_eq!(arg.arg_type, Some(ast::ArgType::Bool));
        assert!(matches!(&arg.default, Some(Expr::BoolLit(false, _))));
        assert_eq!(arg.short.as_ref().unwrap().value, "-v");
    }

    #[test]
    fn parse_multiple_top_level_args() {
        let input = r#"
            arg port { type = string default = "3000" }
            arg verbose { type = bool default = false }
            job web { run "serve" }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.args.len(), 2);
        assert_eq!(file.args[0].name, "port");
        assert_eq!(file.args[1].name, "verbose");
    }

    #[test]
    fn parse_arg_inside_config_fails() {
        let input = r#"
            config {
                arg port { type = string default = "3000" }
            }
        "#;
        let err = parse(input, "test.pman").unwrap_err();
        assert!(
            err.to_string().contains("unexpected token in config block"),
            "got: {}",
            err
        );
    }

    // ── Import tests ──

    #[test]
    fn parse_import_with_alias() {
        let file = parse(r#"import "db/database.pman" as db"#, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].path.value, "db/database.pman");
        assert_eq!(file.imports[0].alias, "db");
    }

    #[test]
    fn parse_import_derived_alias() {
        let file = parse(r#"import "database.pman""#, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].path.value, "database.pman");
        assert_eq!(file.imports[0].alias, "database");
    }

    #[test]
    fn parse_import_derived_alias_with_path() {
        let file = parse(r#"import "lib/utils.pman""#, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].alias, "utils");
    }

    #[test]
    fn parse_import_bad_basename_requires_as() {
        let err = parse(r#"import "my.setup.pman""#, "test.pman").unwrap_err();
        assert!(
            err.to_string().contains("cannot derive alias"),
            "got: {}",
            err
        );
    }

    #[test]
    fn parse_multiple_imports() {
        let input = r#"
            import "db.pman" as db
            import "cache.pman"
            job web { run "serve" }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 2);
        assert_eq!(file.imports[0].alias, "db");
        assert_eq!(file.imports[1].alias, "cache");
        assert_eq!(file.jobs.len(), 1);
    }

    #[test]
    fn parse_namespaced_after() {
        let input = r#"
            job api {
                wait { after @db::migrate }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(
            matches!(&cond.kind, ast::ConditionKind::After { namespace: Some(ns), job } if ns == "db" && job == "migrate")
        );
    }

    #[test]
    fn parse_local_after_unchanged() {
        let input = r#"
            job api {
                wait { after @migrate }
                run "serve"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let cond = &file.jobs[0].body.wait.as_ref().unwrap().conditions[0];
        assert!(
            matches!(&cond.kind, ast::ConditionKind::After { namespace: None, job } if job == "migrate")
        );
    }

    #[test]
    fn parse_namespaced_spawn() {
        let input = r#"
            job web {
                run "serve"
                watch health {
                    http "http://localhost:8080/health"
                    on_fail spawn @db::recovery
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let w = &file.jobs[0].body.watches[0];
        assert!(
            matches!(&w.on_fail, Some(ast::OnFailAction::Spawn(Some(ns), name)) if ns == "db" && name == "recovery")
        );
    }

    #[test]
    fn parse_local_spawn_unchanged() {
        let input = r#"
            job web {
                run "serve"
                watch health {
                    http "http://localhost:8080/health"
                    on_fail spawn @recovery
                }
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        let w = &file.jobs[0].body.watches[0];
        assert!(
            matches!(&w.on_fail, Some(ast::OnFailAction::Spawn(None, name)) if name == "recovery")
        );
    }

    #[test]
    fn parse_import_with_single_binding() {
        let input = r#"import "db.pman" as db { url = args.db_url }"#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 1);
        let imp = &file.imports[0];
        assert_eq!(imp.alias, "db");
        assert_eq!(imp.bindings.len(), 1);
        assert_eq!(imp.bindings[0].name, "url");
        assert!(matches!(&imp.bindings[0].value, Expr::ArgsRef(name, _) if name == "db_url"));
    }

    #[test]
    fn parse_import_with_multiple_bindings() {
        let input = "import \"db.pman\" as db {\n  url = args.db_url\n  port = \"5432\"\n}";
        let file = parse(input, "test.pman").unwrap();
        let imp = &file.imports[0];
        assert_eq!(imp.bindings.len(), 2);
        assert_eq!(imp.bindings[0].name, "url");
        assert!(matches!(&imp.bindings[0].value, Expr::ArgsRef(name, _) if name == "db_url"));
        assert_eq!(imp.bindings[1].name, "port");
        assert!(matches!(&imp.bindings[1].value, Expr::StringLit(s, _) if s == "5432"));
    }

    #[test]
    fn parse_import_without_bindings() {
        let input = r#"import "db.pman" as db"#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.imports.len(), 1);
        assert_eq!(file.imports[0].alias, "db");
        assert!(file.imports[0].bindings.is_empty());
    }

    #[test]
    fn parse_import_derived_alias_with_bindings() {
        let input = r#"import "cache.pman" { port = "6379" }"#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.imports[0].alias, "cache");
        assert_eq!(file.imports[0].bindings.len(), 1);
        assert_eq!(file.imports[0].bindings[0].name, "port");
        assert!(matches!(&file.imports[0].bindings[0].value, Expr::StringLit(s, _) if s == "6379"));
    }

    #[test]
    fn parse_import_with_empty_bindings() {
        let input = r#"import "db.pman" as db {}"#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.imports[0].alias, "db");
        assert!(file.imports[0].bindings.is_empty());
    }

    #[test]
    fn parse_simple_task() {
        let input = r#"task test_a { run "echo hello" }"#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.tasks.len(), 1);
        assert_eq!(file.tasks[0].name, "test_a");
        assert!(file.tasks[0].condition.is_none());
    }

    #[test]
    fn parse_task_with_wait() {
        let input = r#"
            job setup { run "setup" }
            task test_a {
                wait { after @setup }
                run "test"
            }
        "#;
        let file = parse(input, "test.pman").unwrap();
        assert_eq!(file.tasks.len(), 1);
        assert!(file.tasks[0].body.wait.is_some());
    }
}
