use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use anyhow::{Result, bail};

use crate::{
    config::{self, ArgDef, ArgType, Dependency, FileFormat, ForEachConfig, ProcessConfig, Watch},
    pman::{
        ast::{self, BinOp, Expr, RunSection, ShellBlock},
        parser,
        validate,
    },
};

pub fn lower(
    input: &str,
    path: &str,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<ProcessConfig>, Option<String>)> {
    let file = parser::parse(input, path)?;
    validate::validate(&file, path)?;

    let log_dir = file
        .config
        .as_ref()
        .and_then(|c| c.logs.as_ref().map(|l| l.value.clone()));

    // Build base_env: system env + extra_env.
    let mut base_env: HashMap<String, String> = std::env::vars().collect();
    base_env.extend(extra_env.clone());

    // Evaluate global env from config.env bindings.
    let global_env = match &file.config {
        Some(config) => eval_env_bindings(&config.env, arg_values, &HashMap::new(), path)?,
        None => HashMap::new(),
    };

    // Determine skipped jobs/services (those with false conditions).
    let mut skipped_jobs: HashSet<String> = HashSet::new();
    for job in &file.jobs {
        if let Some(cond_expr) = &job.condition
            && !eval_condition_expr(cond_expr, arg_values, path)?
        {
            skipped_jobs.insert(job.name.clone());
        }
    }
    for service in &file.services {
        if let Some(cond_expr) = &service.condition
            && !eval_condition_expr(cond_expr, arg_values, path)?
        {
            skipped_jobs.insert(service.name.clone());
        }
    }

    let mut configs = Vec::new();

    for job in &file.jobs {
        if skipped_jobs.contains(&job.name) {
            continue;
        }
        let mut job_configs = lower_job_or_event(
            &job.name,
            &job.body,
            true,
            true, // jobs default to once=true
            &base_env,
            &global_env,
            arg_values,
            &skipped_jobs,
            path,
        )?;
        configs.append(&mut job_configs);
    }

    for service in &file.services {
        if skipped_jobs.contains(&service.name) {
            continue;
        }
        let mut service_configs = lower_job_or_event(
            &service.name,
            &service.body,
            true,
            false, // services default to once=false
            &base_env,
            &global_env,
            arg_values,
            &skipped_jobs,
            path,
        )?;
        configs.append(&mut service_configs);
    }

    for event in &file.events {
        let mut event_configs = lower_job_or_event(
            &event.name,
            &event.body,
            false,
            false, // events default to once=false
            &base_env,
            &global_env,
            arg_values,
            &skipped_jobs,
            path,
        )?;
        configs.append(&mut event_configs);
    }

    Ok((configs, log_dir))
}

pub fn lower_arg_def(arg: ast::ArgDef) -> Result<ArgDef> {
    let default = match &arg.default {
        Some(expr) => Some(eval_expr_to_string(
            expr,
            &HashMap::new(),
            &HashMap::new(),
            "",
        )?),
        None => None,
    };
    let arg_type = match arg.arg_type {
        Some(ast::ArgType::Bool) => ArgType::Bool,
        Some(ast::ArgType::String) | None => ArgType::String,
    };
    Ok(ArgDef {
        name: arg.name,
        short: arg.short.map(|s| s.value),
        description: arg.description.map(|s| s.value),
        arg_type,
        default,
        env: None,
    })
}

fn eval_expr_to_string(
    expr: &Expr,
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    path: &str,
) -> Result<String> {
    match expr {
        Expr::StringLit(s, _) => Ok(s.clone()),
        Expr::NumberLit(n, _) => {
            if n.fract() == 0.0 {
                Ok(format!("{}", *n as i64))
            } else {
                Ok(format!("{n}"))
            }
        }
        Expr::BoolLit(b, _) => Ok(if *b { "true" } else { "false" }.to_string()),
        Expr::DurationLit(_, span) => {
            bail!(
                "{}",
                span.fmt_error(path, "duration not valid in env context")
            )
        }
        Expr::NoneLit(span) => bail!("{}", span.fmt_error(path, "none not valid in env context")),
        Expr::ArgsRef(name, span) => match arg_values.get(name) {
            Some(v) => Ok(v.clone()),
            None => bail!(
                "{}",
                span.fmt_error(path, &format!("undefined arg: args.{name}"))
            ),
        },
        Expr::JobOutputRef(job, key, _) => Ok(format!("${{{{ {job}.{key} }}}}")),
        Expr::LocalVar(name, span) => match local_vars.get(name) {
            Some(v) => Ok(v.clone()),
            None => bail!(
                "{}",
                span.fmt_error(path, &format!("undefined local variable: {name}"))
            ),
        },
        Expr::BinOp(_, _, _, span) => bail!(
            "{}",
            span.fmt_error(path, "binary expression not valid in env context")
        ),
        Expr::UnaryNot(_, span) => bail!(
            "{}",
            span.fmt_error(path, "unary not not valid in env context")
        ),
    }
}

fn eval_condition_expr(
    expr: &Expr,
    arg_values: &HashMap<String, String>,
    path: &str,
) -> Result<bool> {
    match expr {
        Expr::BoolLit(b, _) => Ok(*b),
        Expr::StringLit(s, _) => Ok(!s.is_empty()),
        Expr::ArgsRef(name, span) => {
            let v = arg_values.get(name).ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    span.fmt_error(path, &format!("undefined arg: args.{name}"))
                )
            })?;
            Ok(v != "false" && v != "0" && !v.is_empty())
        }
        Expr::UnaryNot(inner, _) => Ok(!eval_condition_expr(inner, arg_values, path)?),
        Expr::BinOp(lhs, op, rhs, _) => {
            match op {
                BinOp::And => Ok(eval_condition_expr(lhs, arg_values, path)?
                    && eval_condition_expr(rhs, arg_values, path)?),
                BinOp::Or => Ok(eval_condition_expr(lhs, arg_values, path)?
                    || eval_condition_expr(rhs, arg_values, path)?),
                _ => {
                    // Comparison operators: evaluate both sides as strings and compare.
                    let l = eval_expr_to_string(lhs, arg_values, &HashMap::new(), path)?;
                    let r = eval_expr_to_string(rhs, arg_values, &HashMap::new(), path)?;
                    Ok(match op {
                        BinOp::Eq => l == r,
                        BinOp::Ne => l != r,
                        BinOp::Lt => l < r,
                        BinOp::Gt => l > r,
                        BinOp::Le => l <= r,
                        BinOp::Ge => l >= r,
                        BinOp::And | BinOp::Or => unreachable!(),
                    })
                }
            }
        }
        Expr::NumberLit(_, span) | Expr::LocalVar(_, span) => {
            bail!(
                "{}",
                span.fmt_error(path, "expression type not valid in condition context")
            )
        }
        Expr::DurationLit(_, span) => bail!(
            "{}",
            span.fmt_error(path, "duration not valid in condition context")
        ),
        Expr::NoneLit(span) => bail!(
            "{}",
            span.fmt_error(path, "none not valid in condition context")
        ),
        Expr::JobOutputRef(_, _, span) => bail!(
            "{}",
            span.fmt_error(path, "job output ref not valid in condition context")
        ),
    }
}

fn eval_env_bindings(
    bindings: &[ast::EnvBinding],
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    path: &str,
) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    for binding in bindings {
        let value = eval_expr_to_string(&binding.value, arg_values, local_vars, path)?;
        env.insert(binding.key.clone(), value);
    }
    Ok(env)
}

fn eval_option_timeout(opt: &Option<Expr>, path: &str) -> Result<Option<Duration>> {
    match opt {
        None | Some(Expr::NoneLit(_)) => Ok(None),
        Some(Expr::DurationLit(d, _)) => Ok(Some(Duration::from_secs_f64(*d))),
        Some(other) => bail!(
            "{}",
            other.span().fmt_error(
                path,
                &format!("expected duration or none for timeout, got {other:?}")
            )
        ),
    }
}

fn eval_option_poll(opt: &Option<Expr>, path: &str) -> Result<Option<Duration>> {
    match opt {
        None => Ok(None),
        Some(Expr::DurationLit(d, _)) => Ok(Some(Duration::from_secs_f64(*d))),
        Some(other) => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected duration for poll, got {other:?}"))
        ),
    }
}

fn eval_option_retry(opt: &Option<Expr>, path: &str) -> Result<bool> {
    match opt {
        None => Ok(true),
        Some(Expr::BoolLit(b, _)) => Ok(*b),
        Some(other) => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected bool for retry, got {other:?}"))
        ),
    }
}

fn eval_option_status(opt: &Option<Expr>, path: &str) -> Result<u16> {
    match opt {
        None => Ok(200),
        Some(Expr::NumberLit(n, _)) => Ok(*n as u16),
        Some(other) => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected number for status, got {other:?}"))
        ),
    }
}

fn eval_option_duration(opt: &Option<Expr>, default: Duration, path: &str) -> Result<Duration> {
    match opt {
        None => Ok(default),
        Some(Expr::DurationLit(d, _)) => Ok(Duration::from_secs_f64(*d)),
        Some(other) => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected duration, got {other:?}"))
        ),
    }
}

fn eval_option_u32(opt: &Option<Expr>, default: u32, path: &str) -> Result<u32> {
    match opt {
        None => Ok(default),
        Some(Expr::NumberLit(n, _)) => Ok(*n as u32),
        Some(other) => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected number, got {other:?}"))
        ),
    }
}

fn lower_wait_condition(
    cond: &ast::WaitCondition,
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    path: &str,
) -> Result<Dependency> {
    let opts = &cond.options;
    let timeout = eval_option_timeout(&opts.timeout, path)?;
    let poll = eval_option_poll(&opts.poll, path)?;
    let retry = eval_option_retry(&opts.retry, path)?;

    match (&cond.kind, cond.negated) {
        (ast::ConditionKind::After { job }, false) => Ok(Dependency::ProcessExited {
            name: job.clone(),
            timeout,
            retry,
        }),
        (ast::ConditionKind::Http { url }, false) => {
            let code = eval_option_status(&opts.status, path)?;
            Ok(Dependency::HttpHealthCheck {
                url: eval_string_lit_or_expr(url, arg_values, local_vars)?,
                code,
                poll_interval: poll,
                timeout,
                retry,
            })
        }
        (ast::ConditionKind::Connect { address }, false) => Ok(Dependency::TcpConnect {
            address: eval_string_lit_or_expr(address, arg_values, local_vars)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Connect { address }, true) => Ok(Dependency::TcpNotListening {
            address: eval_string_lit_or_expr(address, arg_values, local_vars)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Exists { path: p }, false) => Ok(Dependency::FileExists {
            path: eval_string_lit_or_expr(p, arg_values, local_vars)?,
            retry,
        }),
        (ast::ConditionKind::Exists { path: p }, true) => Ok(Dependency::FileNotExists {
            path: eval_string_lit_or_expr(p, arg_values, local_vars)?,
            retry,
        }),
        (ast::ConditionKind::Running { pattern }, true) => Ok(Dependency::ProcessNotRunning {
            pattern: eval_string_lit_or_expr(pattern, arg_values, local_vars)?,
            retry,
        }),
        (
            ast::ConditionKind::Contains {
                path: p,
                format,
                key,
                var: _,
            },
            false,
        ) => {
            let file_format = match format.as_str() {
                "json" => FileFormat::Json,
                "yaml" => FileFormat::Yaml,
                other => bail!(
                    "{}",
                    cond.span
                        .fmt_error(path, &format!("unsupported format: {other:?}"))
                ),
            };
            let json_path = serde_json_path::JsonPath::parse(&key.value)
                .map_err(|e| anyhow::anyhow!("invalid JSONPath {:?}: {e}", key.value))?;
            Ok(Dependency::FileContainsKey {
                path: eval_string_lit_or_expr(p, arg_values, local_vars)?,
                format: file_format,
                key: json_path,
                env: None, // Will be wired up in lower_job_or_event.
                poll_interval: poll,
                timeout,
                retry,
            })
        }
        (ast::ConditionKind::After { .. }, true) => {
            bail!(
                "{}",
                cond.span
                    .fmt_error(path, "negated 'after' is not supported")
            )
        }
        (ast::ConditionKind::Http { .. }, true) => {
            bail!(
                "{}",
                cond.span.fmt_error(path, "negated 'http' is not supported")
            )
        }
        (ast::ConditionKind::Running { .. }, false) => {
            bail!(
                "{}",
                cond.span.fmt_error(
                    path,
                    "non-negated 'running' is not supported (use !running)"
                )
            )
        }
        (ast::ConditionKind::Contains { .. }, true) => {
            bail!(
                "{}",
                cond.span
                    .fmt_error(path, "negated 'contains' is not supported")
            )
        }
    }
}

fn eval_string_lit_or_expr(
    lit: &ast::StringLit,
    _arg_values: &HashMap<String, String>,
    _local_vars: &HashMap<String, String>,
) -> Result<String> {
    Ok(lit.value.clone())
}

fn lower_watch(
    watch: &ast::WatchDef,
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    path: &str,
) -> Result<Watch> {
    let check = lower_wait_condition(&watch.condition, arg_values, local_vars, path)?;
    let initial_delay = eval_option_duration(&watch.initial_delay, Duration::ZERO, path)?;
    let poll_interval = eval_option_duration(&watch.poll, Duration::from_secs(5), path)?;
    let failure_threshold = eval_option_u32(&watch.threshold, 3, path)?;
    let on_fail = match &watch.on_fail {
        None => config::OnFailAction::Shutdown,
        Some(ast::OnFailAction::Shutdown) => config::OnFailAction::Shutdown,
        Some(ast::OnFailAction::Debug) => config::OnFailAction::Debug,
        Some(ast::OnFailAction::Log) => config::OnFailAction::Log,
        Some(ast::OnFailAction::Spawn(name)) => config::OnFailAction::Spawn(name.clone()),
    };
    Ok(Watch {
        name: watch.name.clone(),
        check,
        initial_delay,
        poll_interval,
        failure_threshold,
        on_fail,
    })
}

fn extract_shell(shell: &ShellBlock) -> String {
    match shell {
        ShellBlock::Inline(s) => s.value.clone(),
        ShellBlock::Fenced(s, _) => s.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_job_or_event(
    name: &str,
    body: &ast::JobBody,
    autostart: bool,
    once_default: bool,
    base_env: &HashMap<String, String>,
    global_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
    skipped_jobs: &HashSet<String>,
    path: &str,
) -> Result<Vec<ProcessConfig>> {
    let once = once_default;

    // Track var bindings from contains conditions.
    // Maps var_name -> index in depends vec (to be patched with env key).
    let mut var_to_dep_index: HashMap<String, usize> = HashMap::new();
    let local_vars: HashMap<String, String> = HashMap::new();

    // Lower wait conditions -> depends.
    let mut depends = Vec::new();
    if let Some(wait) = &body.wait {
        for cond in &wait.conditions {
            // Strip `after @skipped_job` dependencies.
            if let ast::ConditionKind::After { job } = &cond.kind
                && skipped_jobs.contains(job)
            {
                continue;
            }

            let dep = lower_wait_condition(cond, arg_values, &local_vars, path)?;

            // Track var bindings from contains.
            if let ast::ConditionKind::Contains { var: Some(v), .. } = &cond.kind {
                var_to_dep_index.insert(v.clone(), depends.len());
            }

            depends.push(dep);
        }
    }

    // Wire up contains var bindings to env: find env bindings that reference local vars
    // from contains, and set the FileContainsKey.env field instead.
    let mut env_keys_from_vars: HashMap<String, String> = HashMap::new();
    for binding in &body.env {
        if let Expr::LocalVar(var_name, _) = &binding.value
            && let Some(&dep_idx) = var_to_dep_index.get(var_name)
        {
            if let Dependency::FileContainsKey { env, .. } = &mut depends[dep_idx] {
                *env = Some(binding.key.clone());
            }
            env_keys_from_vars.insert(binding.key.clone(), var_name.clone());
        }
    }

    // Evaluate env bindings (skipping those wired to contains var).
    let mut job_env = HashMap::new();
    for binding in &body.env {
        if env_keys_from_vars.contains_key(&binding.key) {
            continue;
        }
        let value = eval_expr_to_string(&binding.value, arg_values, &local_vars, path)?;
        job_env.insert(binding.key.clone(), value);
    }

    // Merge env: base_env + global_env + job_env (job overrides global overrides base).
    let mut merged_env = base_env.clone();
    merged_env.extend(global_env.clone());
    merged_env.extend(job_env);

    // Lower watches.
    let watches: Vec<Watch> = body
        .watches
        .iter()
        .map(|w| lower_watch(w, arg_values, &local_vars, path))
        .collect::<Result<_>>()?;

    match &body.run_section {
        RunSection::Direct(shell) => Ok(vec![ProcessConfig {
            name: name.to_string(),
            env: merged_env,
            run: extract_shell(shell),
            condition: None,
            depends,
            once,
            for_each: None,
            autostart,
            watches,
        }]),
        RunSection::ForLoop(fl) => match &fl.iterable {
            ast::Iterable::Glob(glob_lit) => {
                // Evaluate for-loop env with the loop var mapped to a
                // placeholder so that expand_fan_out can substitute the
                // real value at runtime.
                let mut placeholder_vars = HashMap::new();
                placeholder_vars.insert(fl.var.clone(), format!("${{{}}}", fl.var));
                let for_env = eval_env_bindings(&fl.env, arg_values, &placeholder_vars, path)?;
                let mut glob_env = merged_env;
                glob_env.extend(for_env);

                Ok(vec![ProcessConfig {
                    name: name.to_string(),
                    env: glob_env,
                    run: extract_shell(&fl.run),
                    condition: None,
                    depends,
                    once,
                    for_each: Some(ForEachConfig {
                        glob: glob_lit.value.clone(),
                        variable: fl.var.clone(),
                    }),
                    autostart,
                    watches,
                }])
            }
            ast::Iterable::Array(items) => {
                let mut configs = Vec::new();
                for (i, item_expr) in items.iter().enumerate() {
                    let item_value = eval_expr_to_string(item_expr, arg_values, &local_vars, path)?;
                    let mut iter_local_vars = HashMap::new();
                    iter_local_vars.insert(fl.var.clone(), item_value);

                    // Evaluate for-loop env with the loop var in scope.
                    let for_env = eval_env_bindings(&fl.env, arg_values, &iter_local_vars, path)?;
                    let mut iter_env = merged_env.clone();
                    iter_env.extend(for_env);

                    configs.push(ProcessConfig {
                        name: format!("{name}-{i}"),
                        env: iter_env,
                        run: extract_shell(&fl.run),
                        condition: None,
                        depends: depends.clone(),
                        once,
                        for_each: None,
                        autostart,
                        watches: watches.clone(),
                    });
                }
                Ok(configs)
            }
            ast::Iterable::RangeExclusive(start_expr, end_expr) => {
                let start = eval_expr_to_number(start_expr, path)? as i64;
                let end = eval_expr_to_number(end_expr, path)? as i64;
                let mut configs = Vec::new();
                for (i, val) in (start..end).enumerate() {
                    let mut iter_local_vars = HashMap::new();
                    iter_local_vars.insert(fl.var.clone(), val.to_string());

                    let for_env = eval_env_bindings(&fl.env, arg_values, &iter_local_vars, path)?;
                    let mut iter_env = merged_env.clone();
                    iter_env.extend(for_env);

                    configs.push(ProcessConfig {
                        name: format!("{name}-{i}"),
                        env: iter_env,
                        run: extract_shell(&fl.run),
                        condition: None,
                        depends: depends.clone(),
                        once,
                        for_each: None,
                        autostart,
                        watches: watches.clone(),
                    });
                }
                Ok(configs)
            }
            ast::Iterable::RangeInclusive(start_expr, end_expr) => {
                let start = eval_expr_to_number(start_expr, path)? as i64;
                let end = eval_expr_to_number(end_expr, path)? as i64;
                let mut configs = Vec::new();
                for (i, val) in (start..=end).enumerate() {
                    let mut iter_local_vars = HashMap::new();
                    iter_local_vars.insert(fl.var.clone(), val.to_string());

                    let for_env = eval_env_bindings(&fl.env, arg_values, &iter_local_vars, path)?;
                    let mut iter_env = merged_env.clone();
                    iter_env.extend(for_env);

                    configs.push(ProcessConfig {
                        name: format!("{name}-{i}"),
                        env: iter_env,
                        run: extract_shell(&fl.run),
                        condition: None,
                        depends: depends.clone(),
                        once,
                        for_each: None,
                        autostart,
                        watches: watches.clone(),
                    });
                }
                Ok(configs)
            }
        },
    }
}

fn eval_expr_to_number(expr: &Expr, path: &str) -> Result<f64> {
    match expr {
        Expr::NumberLit(n, _) => Ok(*n),
        other => bail!(
            "{}",
            other
                .span()
                .fmt_error(path, &format!("expected number, got {other:?}"))
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lower_str(input: &str) -> (Vec<ProcessConfig>, Option<String>) {
        lower(input, "test.pman", &HashMap::new(), &HashMap::new()).unwrap()
    }

    fn lower_with_args(input: &str, args: &[(&str, &str)]) -> (Vec<ProcessConfig>, Option<String>) {
        let arg_values: HashMap<String, String> = args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        lower(input, "test.pman", &HashMap::new(), &arg_values).unwrap()
    }

    #[test]
    fn lower_simple_job() {
        let (configs, _) = lower_str(r#"job web { run "echo hello" }"#);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "web");
        assert_eq!(configs[0].run, "echo hello");
        assert!(configs[0].autostart);
    }

    #[test]
    fn lower_event_is_not_autostart() {
        let (configs, _) = lower_str(r#"event recovery { run "./recover.sh" }"#);
        assert_eq!(configs[0].name, "recovery");
        assert!(!configs[0].autostart);
    }

    #[test]
    fn lower_job_is_once() {
        let (configs, _) = lower_str(r#"job migrate { run "migrate" }"#);
        assert!(configs[0].once);
    }

    #[test]
    fn lower_service_is_not_once() {
        let (configs, _) = lower_str(r#"service web { run "serve" }"#);
        assert!(!configs[0].once);
    }

    #[test]
    fn lower_env_with_args_ref() {
        let (configs, _) = lower_with_args(
            r#"
            config { arg port { type = string default = "3000" } }
            job web { env PORT = args.port run "serve" }
            "#,
            &[("port", "8080")],
        );
        assert_eq!(configs[0].env.get("PORT").unwrap(), "8080");
    }

    #[test]
    fn lower_env_with_job_output_ref() {
        let (configs, _) = lower_str(
            r#"
            job setup { run "setup" }
            job api { wait { after @setup } env DB = @setup.URL run "start" }
        "#,
        );
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.env.get("DB").unwrap(), "${{ setup.URL }}");
    }

    #[test]
    fn lower_wait_after() {
        let (configs, _) = lower_str(
            r#"
            job setup { run "setup" }
            service api { wait { after @setup } run "start" }
        "#,
        );
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.depends.len(), 1);
        match &api.depends[0] {
            Dependency::ProcessExited { name, .. } => assert_eq!(name, "setup"),
            other => panic!("expected ProcessExited, got {other:?}"),
        }
    }

    #[test]
    fn lower_wait_http() {
        let (configs, _) = lower_str(
            r#"
            job api {
                wait {
                    http "http://localhost:8080/health" {
                        status = 200
                        timeout = 30s
                    }
                }
                run "start"
            }
        "#,
        );
        match &configs[0].depends[0] {
            Dependency::HttpHealthCheck {
                url, code, timeout, ..
            } => {
                assert_eq!(url, "http://localhost:8080/health");
                assert_eq!(*code, 200);
                assert_eq!(*timeout, Some(Duration::from_secs(30)));
            }
            other => panic!("expected HttpHealthCheck, got {other:?}"),
        }
    }

    #[test]
    fn lower_timeout_none_is_infinite() {
        let (configs, _) = lower_str(
            r#"
            job setup { run "setup" }
            service api { wait { after @setup { timeout = none } } run "start" }
        "#,
        );
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        match &api.depends[0] {
            Dependency::ProcessExited { timeout, .. } => {
                assert!(
                    timeout.is_none(),
                    "timeout=none should produce None (infinite)"
                );
            }
            other => panic!("expected ProcessExited, got {other:?}"),
        }
    }

    #[test]
    fn lower_retry_false() {
        let (configs, _) = lower_str(
            r#"
            job api { wait { connect "127.0.0.1:5432" { retry = false } } run "start" }
        "#,
        );
        match &configs[0].depends[0] {
            Dependency::TcpConnect { retry, .. } => assert!(!retry),
            other => panic!("expected TcpConnect, got {other:?}"),
        }
    }

    #[test]
    fn lower_negated_connect() {
        let (configs, _) = lower_str(
            r#"
            job api { wait { !connect "127.0.0.1:8080" } run "start" }
        "#,
        );
        match &configs[0].depends[0] {
            Dependency::TcpNotListening { address, .. } => {
                assert_eq!(address, "127.0.0.1:8080");
            }
            other => panic!("expected TcpNotListening, got {other:?}"),
        }
    }

    #[test]
    fn lower_watch() {
        let (configs, _) = lower_str(
            r#"
            event recovery { run "./recover.sh" }
            job web {
                run "serve"
                watch health {
                    http "http://localhost:8080/" { status = 200 }
                    threshold = 3
                    on_fail spawn @recovery
                }
            }
        "#,
        );
        let web = configs.iter().find(|c| c.name == "web").unwrap();
        assert_eq!(web.watches.len(), 1);
        assert_eq!(web.watches[0].name, "health");
    }

    #[test]
    fn lower_conditional_job_false_skipped() {
        let (configs, _) = lower_with_args(
            r#"
            config { arg enabled { type = bool default = false } }
            job worker if args.enabled { run "work" }
            "#,
            &[("enabled", "false")],
        );
        assert!(!configs.iter().any(|c| c.name == "worker"));
    }

    #[test]
    fn lower_conditional_job_true_emitted() {
        let (configs, _) = lower_with_args(
            r#"
            config { arg enabled { type = bool default = false } }
            job worker if args.enabled { run "work" }
            "#,
            &[("enabled", "true")],
        );
        let worker = configs.iter().find(|c| c.name == "worker").unwrap();
        assert!(worker.condition.is_none());
    }

    #[test]
    fn lower_skipped_job_still_allows_after() {
        let (configs, _) = lower_with_args(
            r#"
            config { arg enabled { type = bool default = false } }
            job setup if args.enabled { run "setup" }
            service api { wait { after @setup } run "start" }
            "#,
            &[("enabled", "false")],
        );
        assert!(configs.iter().any(|c| c.name == "api"));
    }

    #[test]
    fn lower_for_array_expands_to_multiple_configs() {
        let (configs, _) = lower_str(
            r#"
            job multi {
                for item in ["a", "b", "c"] {
                    env ITEM = item
                    run "echo $ITEM"
                }
            }
        "#,
        );
        let multi_configs: Vec<_> = configs
            .iter()
            .filter(|c| c.name.starts_with("multi-"))
            .collect();
        assert_eq!(multi_configs.len(), 3);
        assert_eq!(multi_configs[0].name, "multi-0");
        assert_eq!(multi_configs[1].name, "multi-1");
        assert_eq!(multi_configs[2].name, "multi-2");
    }

    #[test]
    fn lower_for_range_expands_to_multiple_configs() {
        let (configs, _) = lower_str(
            r#"
            job workers {
                for i in 0..3 {
                    env WORKER_ID = i
                    run "echo $WORKER_ID"
                }
            }
        "#,
        );
        let worker_configs: Vec<_> = configs
            .iter()
            .filter(|c| c.name.starts_with("workers-"))
            .collect();
        assert_eq!(worker_configs.len(), 3);
    }

    #[test]
    fn lower_for_env_inheritance() {
        let (configs, _) = lower_str(
            r#"
            job nodes {
                env CLUSTER = "prod"
                for item in ["a", "b"] {
                    env NODE = item
                    run "echo"
                }
            }
        "#,
        );
        for c in configs.iter().filter(|c| c.name.starts_with("nodes-")) {
            assert_eq!(c.env.get("CLUSTER").unwrap(), "prod");
        }
    }

    #[test]
    fn lower_global_env_applies_to_all_jobs() {
        let (configs, _) = lower_with_args(
            r#"
            config { env { RUST_LOG = args.log_level } }
            job web { run "serve" }
            job api { run "start" }
            "#,
            &[("log_level", "debug")],
        );
        for c in &configs {
            assert_eq!(c.env.get("RUST_LOG").unwrap(), "debug");
        }
    }

    #[test]
    fn lower_contains_var_binding_to_env() {
        let (configs, _) = lower_str(
            r#"
            job api {
                wait {
                    contains "/tmp/config.yaml" {
                        format = "yaml"
                        key = "$.database.url"
                        var = database_url
                    }
                }
                env DB_URL = database_url
                run "start-api --db $DB_URL"
            }
        "#,
        );
        let api = &configs[0];
        match &api.depends[0] {
            Dependency::FileContainsKey { env, .. } => {
                assert_eq!(env.as_deref(), Some("DB_URL"));
            }
            other => panic!("expected FileContainsKey, got {other:?}"),
        }
    }

    #[test]
    fn lower_per_job_env_overrides_global() {
        let (configs, _) = lower_with_args(
            r#"
            config { env { RUST_LOG = args.log_level } }
            job web { env RUST_LOG = "warn" run "serve" }
            "#,
            &[("log_level", "debug")],
        );
        assert_eq!(configs[0].env.get("RUST_LOG").unwrap(), "warn");
    }

    #[test]
    fn lower_logs_dir() {
        let (_, log_dir) = lower_str(r#"config { logs = "./my-logs" } job web { run "serve" }"#);
        assert_eq!(log_dir.as_deref(), Some("./my-logs"));
    }

    #[test]
    fn full_example_from_spec() {
        let input = r#"
config {
    logs = "./my-logs"

    env {
        RUST_LOG = args.log_level
    }

    arg port {
        type = string
        default = "3000"
        short = "p"
        description = "Port to listen on"
    }

    arg log_level {
        type = string
        default = "info"
        short = "r"
        description = "RUST_LOG configuration"
    }

    arg enable_worker {
        type = bool
        default = false
    }
}

job migrate {
    run "echo done"
}

service web {
    env PORT = args.port
    run "serve --port $PORT"
}

service api {
    env DB_URL = @migrate.DATABASE_URL

    wait {
        after @migrate
        http "http://localhost:3000/health" {
            status = 200
            timeout = 30s
            poll = 500ms
        }
    }

    run "api-server start --db $DB_URL"
}

service db {
    wait {
        connect "127.0.0.1:5432"
    }
    run "db-client start"
}

service healthcheck {
    wait {
        !connect "127.0.0.1:8080"
        !exists "/tmp/api.lock"
        !running "old-api.*"
    }
    run "api-server --port 8080"
}

job worker if args.enable_worker {
    run "worker-service start"
}

service web_watched {
    run "web-server --port 8080"

    watch health {
        http "http://localhost:8080/health" {
            status = 200
        }
        initial_delay = 5s
        poll = 10s
        threshold = 3
        on_fail shutdown
    }

    watch disk {
        exists "/var/run/healthy"
        on_fail spawn @recovery
    }
}

event recovery {
    run "./scripts/recover.sh"
}
    "#;

        let args = HashMap::from([
            ("port".to_string(), "3000".to_string()),
            ("log_level".to_string(), "info".to_string()),
            ("enable_worker".to_string(), "false".to_string()),
        ]);
        let (configs, log_dir) = lower(input, "test.pman", &HashMap::new(), &args).unwrap();

        assert_eq!(log_dir.as_deref(), Some("./my-logs"));

        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"migrate"));
        assert!(names.contains(&"web"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db"));
        assert!(names.contains(&"healthcheck"));
        assert!(!names.contains(&"worker")); // enable_worker=false
        assert!(names.contains(&"web_watched"));
        assert!(names.contains(&"recovery"));

        let migrate = configs.iter().find(|c| c.name == "migrate").unwrap();
        assert!(migrate.once);

        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.depends.len(), 2);
        assert_eq!(
            api.env.get("DB_URL").unwrap(),
            "${{ migrate.DATABASE_URL }}"
        );

        let recovery = configs.iter().find(|c| c.name == "recovery").unwrap();
        assert!(!recovery.autostart);

        let web_watched = configs.iter().find(|c| c.name == "web_watched").unwrap();
        assert_eq!(web_watched.watches.len(), 2);
    }
}
