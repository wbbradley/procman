use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    time::Duration,
};

use anyhow::{Result, bail};

use crate::{
    config::{self, ArgDef, ArgType, Dependency, FileFormat, ForEachConfig, ProcessConfig, Watch},
    pman::{
        ast::{self, BinOp, Expr, RunSection, ShellBlock},
        loader::LoadedModules,
        validate,
    },
};

pub type ModuleArgsReport = Vec<(String, BTreeMap<String, String>)>;

pub fn parent_dir_of(path: &str) -> String {
    let parent = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::canonicalize(parent)
        .unwrap_or_else(|_| parent.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn qualify_name(prefix: Option<&str>, namespace: Option<&str>, name: &str) -> String {
    match (prefix, namespace) {
        (Some(p), Some(ns)) => format!("{p}::{ns}::{name}"),
        (None, Some(ns)) => format!("{ns}::{name}"),
        (Some(p), None) => format!("{p}::{name}"),
        (None, None) => name.to_string(),
    }
}

/// Recursively validate a module and all its sub-imports.
fn validate_recursive(module: &crate::pman::loader::LoadedModule) -> Result<()> {
    validate::validate(&module.file, &module.path)?;
    for sub in module.imports.values() {
        validate_recursive(sub)?;
    }
    Ok(())
}

/// Flatten the module tree into a list of `(compound_prefix, &LoadedModule)` pairs,
/// resolving args recursively. Each module's resolved args are stored in `per_module_args`
/// keyed by compound prefix.
fn collect_modules_and_resolve_args<'a>(
    module: &'a crate::pman::loader::LoadedModule,
    prefix: &str,
    parent_arg_values: &HashMap<String, String>,
    parent_path: &str,
    procman_dir: &str,
    per_module_args: &mut HashMap<String, HashMap<String, String>>,
    all_modules: &mut Vec<(String, &'a crate::pman::loader::LoadedModule)>,
) -> Result<()> {
    let resolved = resolve_module_args(module, parent_arg_values, parent_path, procman_dir)?;

    // Recurse into sub-imports, using this module's resolved args as parent context.
    for (sub_alias, sub_module) in &module.imports {
        let sub_prefix = format!("{prefix}::{sub_alias}");
        collect_modules_and_resolve_args(
            sub_module,
            &sub_prefix,
            &resolved,
            &module.path,
            procman_dir,
            per_module_args,
            all_modules,
        )?;
    }

    // Extend this module's arg map with sub_alias::name entries (for NamespacedArgsRef).
    let mut extended = resolved.clone();
    for (sub_alias, sub_module) in &module.imports {
        let sub_prefix = format!("{prefix}::{sub_alias}");
        if let Some(sub_args) = per_module_args.get(&sub_prefix) {
            for (name, value) in sub_args {
                extended.insert(format!("{sub_alias}::{name}"), value.clone());
            }
        }
        // Also register sub-module arg names directly (for the sub_alias::args.name pattern).
        let _ = sub_module;
    }

    per_module_args.insert(prefix.to_string(), extended);
    all_modules.push((prefix.to_string(), module));
    Ok(())
}

pub fn lower_modules(
    modules: &LoadedModules,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<ProcessConfig>, Option<String>, ModuleArgsReport)> {
    // Validate root and all imported files recursively.
    validate::validate(&modules.root, &modules.root_path)?;
    for module in modules.imports.values() {
        validate_recursive(module)?;
    }
    // Cross-module validation.
    validate::validate_cross_refs(modules)?;

    // Flatten all imports recursively, resolving args at each level.
    let root_dir = parent_dir_of(&modules.root_path);
    let mut per_module_args: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut all_modules: Vec<(String, &crate::pman::loader::LoadedModule)> = Vec::new();
    for (alias, module) in &modules.imports {
        collect_modules_and_resolve_args(
            module,
            alias,
            arg_values,
            &modules.root_path,
            &root_dir,
            &mut per_module_args,
            &mut all_modules,
        )?;
    }

    // Build extended root args (includes namespaced entries for direct imports only).
    let mut root_args = arg_values.clone();
    root_args.insert("__procman_dir__".to_string(), root_dir.clone());
    root_args.insert("__module_dir__".to_string(), root_dir.clone());
    for alias in modules.imports.keys() {
        if let Some(mod_args) = per_module_args.get(alias.as_str()) {
            for (name, value) in mod_args {
                root_args.insert(format!("{alias}::{name}"), value.clone());
            }
        }
    }

    // Inject __procman_dir__ into every imported module's arg map.
    for args in per_module_args.values_mut() {
        args.insert("__procman_dir__".to_string(), root_dir.clone());
    }

    let log_dir = modules
        .root
        .config
        .as_ref()
        .and_then(|c| c.logs.as_ref().map(|l| l.value.clone()));

    // Build base_env: system env + extra_env.
    let mut base_env: HashMap<String, String> = std::env::vars().collect();
    base_env.extend(extra_env.clone());

    // Compute global skipped jobs across all modules.
    let mut skipped_jobs: HashSet<String> = HashSet::new();
    collect_skipped_jobs(
        &modules.root,
        None,
        &root_args,
        &modules.root_path,
        &mut skipped_jobs,
    )?;
    for (prefix, module) in &all_modules {
        collect_skipped_jobs(
            &module.file,
            Some(prefix.as_str()),
            &per_module_args[prefix],
            &module.path,
            &mut skipped_jobs,
        )?;
    }

    let mut configs = Vec::new();

    // Lower root entities (no prefix).
    lower_file_entities(
        &modules.root,
        &modules.root_path,
        None,
        &base_env,
        &root_args,
        &skipped_jobs,
        &mut configs,
    )?;

    // Lower each flattened module's entities (with compound prefix).
    for (prefix, module) in &all_modules {
        lower_file_entities(
            &module.file,
            &module.path,
            Some(prefix.as_str()),
            &base_env,
            &per_module_args[prefix],
            &skipped_jobs,
            &mut configs,
        )?;
    }

    // Build sorted module args report (excluding namespaced forwarding entries).
    let mut module_args_report: Vec<(String, BTreeMap<String, String>)> = Vec::new();
    let root_report: BTreeMap<String, String> = root_args
        .iter()
        .filter(|(k, _)| !k.contains("::"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    module_args_report.push(("(root)".to_string(), root_report));
    for (prefix, args) in &per_module_args {
        let report: BTreeMap<String, String> = args
            .iter()
            .filter(|(k, _)| !k.contains("::"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        module_args_report.push((prefix.clone(), report));
    }
    module_args_report.sort_by(|(a, _), (b, _)| a.cmp(b));

    Ok((configs, log_dir, module_args_report))
}

fn collect_skipped_jobs(
    file: &ast::File,
    prefix: Option<&str>,
    arg_values: &HashMap<String, String>,
    path: &str,
    skipped_jobs: &mut HashSet<String>,
) -> Result<()> {
    for job in &file.jobs {
        if let Some(cond_expr) = &job.condition
            && !eval_condition_expr(cond_expr, arg_values, path)?
        {
            skipped_jobs.insert(qualify_name(prefix, None, &job.name));
        }
    }
    for service in &file.services {
        if let Some(cond_expr) = &service.condition
            && !eval_condition_expr(cond_expr, arg_values, path)?
        {
            skipped_jobs.insert(qualify_name(prefix, None, &service.name));
        }
    }
    for task in &file.tasks {
        if let Some(cond_expr) = &task.condition
            && !eval_condition_expr(cond_expr, arg_values, path)?
        {
            skipped_jobs.insert(qualify_name(prefix, None, &task.name));
        }
    }
    Ok(())
}

fn collect_arg_deps(expr: &Expr, deps: &mut HashSet<String>) {
    match expr {
        Expr::ArgsRef(name, _) => {
            if !name.starts_with("__") {
                deps.insert(name.clone());
            }
        }
        Expr::BinOp(lhs, _, rhs, _) => {
            collect_arg_deps(lhs, deps);
            collect_arg_deps(rhs, deps);
        }
        Expr::UnaryNot(inner, _) => collect_arg_deps(inner, deps),
        _ => {}
    }
}

pub fn topo_sort_args(args: &[ast::ArgDef]) -> Result<Vec<usize>> {
    let name_to_idx: HashMap<&str, usize> = args
        .iter()
        .enumerate()
        .map(|(i, a)| (a.name.as_str(), i))
        .collect();

    let deps: Vec<HashSet<usize>> = args
        .iter()
        .map(|arg| {
            arg.default
                .as_ref()
                .map(|expr| {
                    let mut names = HashSet::new();
                    collect_arg_deps(expr, &mut names);
                    names
                        .iter()
                        .filter_map(|name| name_to_idx.get(name.as_str()).copied())
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect();

    let mut order = Vec::with_capacity(args.len());
    let mut state = vec![0u8; args.len()]; // 0=unvisited, 1=visiting, 2=done

    fn dfs(
        idx: usize,
        deps: &[HashSet<usize>],
        state: &mut [u8],
        order: &mut Vec<usize>,
        args: &[ast::ArgDef],
    ) -> Result<()> {
        if state[idx] == 2 {
            return Ok(());
        }
        if state[idx] == 1 {
            bail!("cyclical arg defaults involving '{}'", args[idx].name);
        }
        state[idx] = 1;
        for &dep in &deps[idx] {
            dfs(dep, deps, state, order, args)?;
        }
        state[idx] = 2;
        order.push(idx);
        Ok(())
    }

    for i in 0..args.len() {
        dfs(i, &deps, &mut state, &mut order, args)?;
    }
    Ok(order)
}

fn resolve_module_args(
    module: &crate::pman::loader::LoadedModule,
    root_arg_values: &HashMap<String, String>,
    root_path: &str,
    procman_dir: &str,
) -> Result<HashMap<String, String>> {
    let mut resolved = HashMap::new();

    // Inject dir vars so defaults can reference them.
    resolved.insert("__module_dir__".to_string(), parent_dir_of(&module.path));
    resolved.insert("__procman_dir__".to_string(), procman_dir.to_string());

    // Apply defaults in topological order so inter-arg refs work.
    let sorted = topo_sort_args(&module.file.args)?;
    for idx in sorted {
        let arg_def = &module.file.args[idx];
        if let Some(ref default_expr) = arg_def.default {
            let value =
                eval_expr_to_string(default_expr, &resolved, &HashMap::new(), None, &module.path)?;
            resolved.insert(arg_def.name.clone(), value);
        }
    }

    // Override with import-site bindings (evaluated in root context).
    for binding in &module.bindings {
        let value = eval_expr_to_string(
            &binding.value,
            root_arg_values,
            &HashMap::new(),
            None,
            root_path,
        )?;
        resolved.insert(binding.name.clone(), value);
    }

    // Override with CLI values (highest priority).
    for arg_def in &module.file.args {
        let cli_key = format!("{}::{}", module.alias, arg_def.name);
        if let Some(value) = root_arg_values.get(&cli_key) {
            resolved.insert(arg_def.name.clone(), value.clone());
        }
    }

    // Error on unbound args (no binding, no default, no CLI override).
    for arg_def in &module.file.args {
        if !resolved.contains_key(&arg_def.name) {
            bail!(
                "{}",
                arg_def.span.fmt_error(
                    &module.path,
                    &format!(
                        "imported module '{}': arg '{}' has no import-site binding and no default",
                        module.alias, arg_def.name
                    ),
                )
            );
        }
    }

    Ok(resolved)
}

fn lower_file_entities(
    file: &ast::File,
    path: &str,
    prefix: Option<&str>,
    base_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
    skipped_jobs: &HashSet<String>,
    configs: &mut Vec<ProcessConfig>,
) -> Result<()> {
    // Evaluate this module's global env bindings.
    let global_env = eval_env_bindings(&file.env, arg_values, &HashMap::new(), prefix, path)?;

    for job in &file.jobs {
        let qualified = qualify_name(prefix, None, &job.name);
        if skipped_jobs.contains(&qualified) {
            continue;
        }
        let mut job_configs = lower_job_or_event(
            &qualified,
            &job.body,
            true,
            true,  // jobs default to once=true
            false, // not a task
            base_env,
            &global_env,
            arg_values,
            skipped_jobs,
            prefix,
            path,
        )?;
        configs.append(&mut job_configs);
    }

    for service in &file.services {
        let qualified = qualify_name(prefix, None, &service.name);
        if skipped_jobs.contains(&qualified) {
            continue;
        }
        let mut service_configs = lower_job_or_event(
            &qualified,
            &service.body,
            true,
            false, // services default to once=false
            false, // not a task
            base_env,
            &global_env,
            arg_values,
            skipped_jobs,
            prefix,
            path,
        )?;
        configs.append(&mut service_configs);
    }

    for event in &file.events {
        let qualified = qualify_name(prefix, None, &event.name);
        let mut event_configs = lower_job_or_event(
            &qualified,
            &event.body,
            false,
            false, // events default to once=false
            false, // not a task
            base_env,
            &global_env,
            arg_values,
            skipped_jobs,
            prefix,
            path,
        )?;
        configs.append(&mut event_configs);
    }

    for task in &file.tasks {
        let qualified = qualify_name(prefix, None, &task.name);
        if skipped_jobs.contains(&qualified) {
            continue;
        }
        let mut task_configs = lower_job_or_event(
            &qualified,
            &task.body,
            false, // tasks do not autostart
            true,  // tasks run to completion
            true,  // is a task
            base_env,
            &global_env,
            arg_values,
            skipped_jobs,
            prefix,
            path,
        )?;
        configs.append(&mut task_configs);
    }

    Ok(())
}

/// Backward-compatible wrapper used by tests.
#[cfg(test)]
pub fn lower(
    input: &str,
    path: &str,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<ProcessConfig>, Option<String>)> {
    use crate::pman::parser;
    let file = parser::parse(input, path)?;
    let modules = LoadedModules {
        root: file,
        root_path: path.to_string(),
        imports: HashMap::new(),
    };
    let (configs, log_dir, _) = lower_modules(&modules, extra_env, arg_values)?;
    Ok((configs, log_dir))
}

pub fn lower_arg_def_ref(
    arg: &ast::ArgDef,
    namespace: Option<&str>,
    dir_context: &HashMap<String, String>,
) -> Result<ArgDef> {
    let default = match &arg.default {
        Some(expr) => Some(eval_expr_to_string(
            expr,
            dir_context,
            &HashMap::new(),
            None,
            "",
        )?),
        None => None,
    };
    let arg_type = match arg.arg_type {
        Some(ast::ArgType::Bool) => ArgType::Bool,
        Some(ast::ArgType::String) | None => ArgType::String,
    };
    Ok(ArgDef {
        name: arg.name.clone(),
        namespace: namespace.map(|s| s.to_string()),
        short: arg.short.as_ref().and_then(|s| {
            if namespace.is_none() {
                Some(s.value.clone())
            } else {
                None
            }
        }),
        description: arg.description.as_ref().map(|s| s.value.clone()),
        arg_type,
        default,
        env: None,
    })
}

fn eval_expr_to_string(
    expr: &Expr,
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    prefix: Option<&str>,
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
        Expr::NamespacedArgsRef(ns, name, span) => {
            let key = format!("{ns}::{name}");
            match arg_values.get(&key) {
                Some(v) => Ok(v.clone()),
                None => bail!(
                    "{}",
                    span.fmt_error(
                        path,
                        &format!("undefined namespaced arg: {ns}::args.{name}")
                    )
                ),
            }
        }
        Expr::JobOutputRef(ns, job, key, _) => {
            let qualified = qualify_name(prefix, ns.as_deref(), job);
            Ok(format!("${{{{ {qualified}.{key} }}}}"))
        }
        Expr::LocalVar(name, span) => match local_vars.get(name) {
            Some(v) => Ok(v.clone()),
            None => bail!(
                "{}",
                span.fmt_error(path, &format!("undefined local variable: {name}"))
            ),
        },
        Expr::BinOp(lhs, BinOp::Concat, rhs, _) => {
            let l = eval_expr_to_string(lhs, arg_values, local_vars, prefix, path)?;
            let r = eval_expr_to_string(rhs, arg_values, local_vars, prefix, path)?;
            Ok(l + &r)
        }
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
        Expr::BinOp(lhs, op, rhs, span) => {
            match op {
                BinOp::And => Ok(eval_condition_expr(lhs, arg_values, path)?
                    && eval_condition_expr(rhs, arg_values, path)?),
                BinOp::Or => Ok(eval_condition_expr(lhs, arg_values, path)?
                    || eval_condition_expr(rhs, arg_values, path)?),
                BinOp::Concat => {
                    let s = eval_expr_to_string(
                        &Expr::BinOp(lhs.clone(), BinOp::Concat, rhs.clone(), *span),
                        arg_values,
                        &HashMap::new(),
                        None,
                        path,
                    )?;
                    Ok(!s.is_empty())
                }
                _ => {
                    // Comparison operators: evaluate both sides as strings and compare.
                    let l = eval_expr_to_string(lhs, arg_values, &HashMap::new(), None, path)?;
                    let r = eval_expr_to_string(rhs, arg_values, &HashMap::new(), None, path)?;
                    Ok(match op {
                        BinOp::Eq => l == r,
                        BinOp::Ne => l != r,
                        BinOp::Lt => l < r,
                        BinOp::Gt => l > r,
                        BinOp::Le => l <= r,
                        BinOp::Ge => l >= r,
                        BinOp::And | BinOp::Or | BinOp::Concat => unreachable!(),
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
        Expr::JobOutputRef(_, _, _, span) => bail!(
            "{}",
            span.fmt_error(path, "job output ref not valid in condition context")
        ),
        Expr::NamespacedArgsRef(ns, name, span) => {
            let key = format!("{ns}::{name}");
            let v = arg_values.get(&key).ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    span.fmt_error(
                        path,
                        &format!("undefined namespaced arg: {ns}::args.{name}")
                    )
                )
            })?;
            Ok(v != "false" && v != "0" && !v.is_empty())
        }
    }
}

fn eval_env_bindings(
    bindings: &[ast::EnvBinding],
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    prefix: Option<&str>,
    path: &str,
) -> Result<HashMap<String, String>> {
    let mut env = HashMap::new();
    for binding in bindings {
        let value = eval_expr_to_string(&binding.value, arg_values, local_vars, prefix, path)?;
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
    prefix: Option<&str>,
    path: &str,
) -> Result<Dependency> {
    let opts = &cond.options;
    let timeout = eval_option_timeout(&opts.timeout, path)?;
    let poll = eval_option_poll(&opts.poll, path)?;
    let retry = eval_option_retry(&opts.retry, path)?;

    match (&cond.kind, cond.negated) {
        (ast::ConditionKind::After { namespace, job }, false) => {
            let qualified = qualify_name(prefix, namespace.as_deref(), job);
            Ok(Dependency::ProcessExited {
                name: qualified,
                poll_interval: poll,
                timeout,
                retry,
            })
        }
        (ast::ConditionKind::Http { url }, false) => {
            let code = eval_option_status(&opts.status, path)?;
            Ok(Dependency::HttpHealthCheck {
                url: eval_string_lit_or_expr(url, arg_values, local_vars, path)?,
                code,
                poll_interval: poll,
                timeout,
                retry,
            })
        }
        (ast::ConditionKind::Connect { address }, false) => Ok(Dependency::TcpConnect {
            address: eval_string_lit_or_expr(address, arg_values, local_vars, path)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Connect { address }, true) => Ok(Dependency::TcpNotListening {
            address: eval_string_lit_or_expr(address, arg_values, local_vars, path)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Exists { path: p }, false) => Ok(Dependency::FileExists {
            path: eval_string_lit_or_expr(p, arg_values, local_vars, path)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Exists { path: p }, true) => Ok(Dependency::FileNotExists {
            path: eval_string_lit_or_expr(p, arg_values, local_vars, path)?,
            poll_interval: poll,
            timeout,
            retry,
        }),
        (ast::ConditionKind::Running { pattern }, true) => Ok(Dependency::ProcessNotRunning {
            pattern: eval_string_lit_or_expr(pattern, arg_values, local_vars, path)?,
            poll_interval: poll,
            timeout,
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
                path: eval_string_lit_or_expr(p, arg_values, local_vars, path)?,
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
    arg_values: &HashMap<String, String>,
    _local_vars: &HashMap<String, String>,
    path: &str,
) -> Result<String> {
    let raw = &lit.value;
    let mut result = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            anyhow::anyhow!("{}", lit.span.fmt_error(path, "unterminated ${} reference"))
        })?;
        let body = after[..end].trim();
        let key = if let Some(name) = body.strip_prefix("args.") {
            name.to_string()
        } else if body == "module.dir" {
            "__module_dir__".to_string()
        } else if body == "procman.dir" {
            "__procman_dir__".to_string()
        } else if let Some((alias, tail)) = body.split_once("::") {
            if let Some(name) = tail.strip_prefix("args.") {
                format!("{alias}::{name}")
            } else if tail == "module.dir" {
                format!("{alias}::__module_dir__")
            } else {
                bail!(
                    "{}",
                    lit.span
                        .fmt_error(path, &format!("unknown reference '${{{body}}}' in string"))
                );
            }
        } else {
            bail!(
                "{}",
                lit.span
                    .fmt_error(path, &format!("unknown reference '${{{body}}}' in string"))
            );
        };
        let value = arg_values.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                lit.span
                    .fmt_error(path, &format!("no value for '${{{body}}}' in string"))
            )
        })?;
        result.push_str(value);
        rest = &after[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

fn lower_watch(
    watch: &ast::WatchDef,
    arg_values: &HashMap<String, String>,
    local_vars: &HashMap<String, String>,
    prefix: Option<&str>,
    path: &str,
) -> Result<Watch> {
    let check = lower_wait_condition(&watch.condition, arg_values, local_vars, prefix, path)?;
    let initial_delay = eval_option_duration(&watch.initial_delay, Duration::ZERO, path)?;
    let poll_interval = eval_option_duration(&watch.poll, Duration::from_secs(5), path)?;
    let failure_threshold = eval_option_u32(&watch.threshold, 3, path)?;
    let on_fail = match &watch.on_fail {
        None => config::OnFailAction::Shutdown,
        Some(ast::OnFailAction::Shutdown) => config::OnFailAction::Shutdown,
        Some(ast::OnFailAction::Debug) => config::OnFailAction::Debug,
        Some(ast::OnFailAction::Log) => config::OnFailAction::Log,
        Some(ast::OnFailAction::Spawn(ns, name)) => {
            config::OnFailAction::Spawn(qualify_name(prefix, ns.as_deref(), name))
        }
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
    is_task: bool,
    base_env: &HashMap<String, String>,
    global_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
    skipped_jobs: &HashSet<String>,
    prefix: Option<&str>,
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
            if let ast::ConditionKind::After { namespace, job } = &cond.kind {
                let qualified = qualify_name(prefix, namespace.as_deref(), job);
                if skipped_jobs.contains(&qualified) {
                    continue;
                }
            }

            let dep = lower_wait_condition(cond, arg_values, &local_vars, prefix, path)?;

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
        let value = eval_expr_to_string(&binding.value, arg_values, &local_vars, prefix, path)?;
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
        .map(|w| lower_watch(w, arg_values, &local_vars, prefix, path))
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
            is_task,
        }]),
        RunSection::ForLoop(fl) => match &fl.iterable {
            ast::Iterable::Glob(glob_lit) => {
                let for_each = ForEachConfig::Glob {
                    pattern: glob_lit.value.clone(),
                    variable: fl.var.clone(),
                };
                // Evaluate for-loop env with the loop var mapped to a
                // placeholder so that expand_fan_out can substitute the
                // real value at runtime.
                let mut placeholder_vars = HashMap::new();
                placeholder_vars.insert(fl.var.clone(), format!("${{{}}}", fl.var));
                let for_env =
                    eval_env_bindings(&fl.env, arg_values, &placeholder_vars, prefix, path)?;
                let mut deferred_env = merged_env;
                deferred_env.extend(for_env);

                Ok(vec![ProcessConfig {
                    name: name.to_string(),
                    env: deferred_env,
                    run: extract_shell(&fl.run),
                    condition: None,
                    depends,
                    once,
                    for_each: Some(for_each),
                    autostart,
                    watches,
                    is_task,
                }])
            }
            ast::Iterable::Array(items) => {
                let values: Vec<String> = items
                    .iter()
                    .map(|e| eval_expr_to_string(e, arg_values, &local_vars, prefix, path))
                    .collect::<Result<_>>()?;
                let for_each = ForEachConfig::Array {
                    values,
                    variable: fl.var.clone(),
                };
                let mut placeholder_vars = HashMap::new();
                placeholder_vars.insert(fl.var.clone(), format!("${{{}}}", fl.var));
                let for_env =
                    eval_env_bindings(&fl.env, arg_values, &placeholder_vars, prefix, path)?;
                let mut deferred_env = merged_env;
                deferred_env.extend(for_env);

                Ok(vec![ProcessConfig {
                    name: name.to_string(),
                    env: deferred_env,
                    run: extract_shell(&fl.run),
                    condition: None,
                    depends,
                    once,
                    for_each: Some(for_each),
                    autostart,
                    watches,
                    is_task,
                }])
            }
            ast::Iterable::RangeExclusive(start_expr, end_expr) => {
                let start = eval_expr_to_number(start_expr, path)? as i64;
                let end = eval_expr_to_number(end_expr, path)? as i64;
                let for_each = ForEachConfig::Range {
                    start,
                    end,
                    inclusive: false,
                    variable: fl.var.clone(),
                };
                let mut placeholder_vars = HashMap::new();
                placeholder_vars.insert(fl.var.clone(), format!("${{{}}}", fl.var));
                let for_env =
                    eval_env_bindings(&fl.env, arg_values, &placeholder_vars, prefix, path)?;
                let mut deferred_env = merged_env;
                deferred_env.extend(for_env);

                Ok(vec![ProcessConfig {
                    name: name.to_string(),
                    env: deferred_env,
                    run: extract_shell(&fl.run),
                    condition: None,
                    depends,
                    once,
                    for_each: Some(for_each),
                    autostart,
                    watches,
                    is_task,
                }])
            }
            ast::Iterable::RangeInclusive(start_expr, end_expr) => {
                let start = eval_expr_to_number(start_expr, path)? as i64;
                let end = eval_expr_to_number(end_expr, path)? as i64;
                let for_each = ForEachConfig::Range {
                    start,
                    end,
                    inclusive: true,
                    variable: fl.var.clone(),
                };
                let mut placeholder_vars = HashMap::new();
                placeholder_vars.insert(fl.var.clone(), format!("${{{}}}", fl.var));
                let for_env =
                    eval_env_bindings(&fl.env, arg_values, &placeholder_vars, prefix, path)?;
                let mut deferred_env = merged_env;
                deferred_env.extend(for_env);

                Ok(vec![ProcessConfig {
                    name: name.to_string(),
                    env: deferred_env,
                    run: extract_shell(&fl.run),
                    condition: None,
                    depends,
                    once,
                    for_each: Some(for_each),
                    autostart,
                    watches,
                    is_task,
                }])
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
            arg port { type = string default = "3000" }
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
            arg enabled { type = bool default = false }
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
            arg enabled { type = bool default = false }
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
            arg enabled { type = bool default = false }
            job setup if args.enabled { run "setup" }
            service api { wait { after @setup } run "start" }
            "#,
            &[("enabled", "false")],
        );
        assert!(configs.iter().any(|c| c.name == "api"));
    }

    #[test]
    fn lower_for_array_defers_expansion() {
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
        let multi = configs.iter().find(|c| c.name == "multi").unwrap();
        match multi.for_each.as_ref().unwrap() {
            ForEachConfig::Array { values, variable } => {
                assert_eq!(values, &["a", "b", "c"]);
                assert_eq!(variable, "item");
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn lower_for_range_defers_expansion() {
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
        let workers = configs.iter().find(|c| c.name == "workers").unwrap();
        match workers.for_each.as_ref().unwrap() {
            ForEachConfig::Range {
                start,
                end,
                inclusive,
                variable,
            } => {
                assert_eq!(*start, 0);
                assert_eq!(*end, 3);
                assert!(!inclusive);
                assert_eq!(variable, "i");
            }
            other => panic!("expected Range, got {other:?}"),
        }
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
        let nodes = configs.iter().find(|c| c.name == "nodes").unwrap();
        assert_eq!(nodes.env.get("CLUSTER").unwrap(), "prod");
        assert!(nodes.for_each.is_some());
    }

    #[test]
    fn lower_global_env_applies_to_all_jobs() {
        let (configs, _) = lower_with_args(
            r#"
            env { RUST_LOG = args.log_level }
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
            env { RUST_LOG = args.log_level }
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
}

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

    #[test]
    fn lower_imported_job_prefixed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"web"), "names: {names:?}");
        assert!(names.contains(&"db::migrate"), "names: {names:?}");
    }

    #[test]
    fn lower_namespaced_after_dep() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                wait { after @db::migrate }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.depends.len(), 1);
        match &api.depends[0] {
            Dependency::ProcessExited { name, .. } => {
                assert_eq!(name, "db::migrate");
            }
            other => panic!("expected ProcessExited, got {other:?}"),
        }
    }

    #[test]
    fn lower_namespaced_output_ref() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                wait { after @db::migrate }
                env DB_URL = @db::migrate.DATABASE_URL
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(
            api.env.get("DB_URL").unwrap(),
            "${{ db::migrate.DATABASE_URL }}"
        );
    }

    #[test]
    fn lower_env_scoping() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            env { MODULE_VAR = "from_db" }
            job migrate { run "migrate" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            env { ROOT_VAR = "from_root" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let web = configs.iter().find(|c| c.name == "web").unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        // Root job has ROOT_VAR but not MODULE_VAR.
        assert_eq!(web.env.get("ROOT_VAR").unwrap(), "from_root");
        assert!(!web.env.contains_key("MODULE_VAR"));
        // Imported job has MODULE_VAR but not ROOT_VAR.
        assert_eq!(migrate.env.get("MODULE_VAR").unwrap(), "from_db");
        assert!(!migrate.env.contains_key("ROOT_VAR"));
    }

    #[test]
    fn lower_skipped_imported_job() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg enabled { type = bool default = false }
            job migrate if args.enabled { run "migrate" }
            job other { run "other" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                wait { after @db::migrate }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let args = HashMap::from([("enabled".to_string(), "false".to_string())]);
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &args).unwrap();
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        // migrate is skipped; api still exists but without the after dep.
        assert!(!names.contains(&"db::migrate"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"db::other"));
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert!(api.depends.is_empty());
    }

    #[test]
    fn lower_import_binding_literal() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "pg://localhost" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "pg://localhost");
    }

    #[test]
    fn lower_import_binding_from_root_arg() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            arg db_url { type = string default = "pg://mydb" }
            import "db.pman" as db { url = args.db_url }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let args = HashMap::from([("db_url".to_string(), "pg://mydb".to_string())]);
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &args).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "pg://mydb");
    }

    #[test]
    fn lower_namespaced_args_ref_in_env() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate { run "migrate" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "pg://x" }
            service api {
                env { DB = db::args.url }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let api = configs.iter().find(|c| c.name == "api").unwrap();
        assert_eq!(api.env.get("DB").unwrap(), "pg://x");
    }

    #[test]
    fn lower_namespaced_args_ref_in_condition() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg enabled { type = bool default = true }
            job migrate { run "migrate" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { enabled = "false" }
            service api if db::args.enabled { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert!(
            !names.contains(&"api"),
            "api should be skipped; got: {names:?}"
        );
    }

    #[test]
    fn lower_unbound_imported_arg_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate { run "migrate" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let err = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(
            err.to_string()
                .contains("no import-site binding and no default"),
            "got: {err}"
        );
    }

    #[test]
    fn lower_import_binding_overrides_default() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string default = "old" }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "new" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "new");
    }

    #[test]
    fn lower_cli_override_unbound_arg() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let mut arg_values = HashMap::new();
        arg_values.insert("db::url".to_string(), "postgres://cli".to_string());
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &arg_values).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "postgres://cli");
    }

    #[test]
    fn lower_multiple_imports_separate_args() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let cache_path = dir.path().join("cache.pman");
        std::fs::write(
            &cache_path,
            r#"
            arg host { type = string }
            service redis {
                env { REDIS = args.host }
                run "redis-cli"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "pg://db" }
            import "cache.pman" as cache { host = "redis://cache" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        let redis = configs.iter().find(|c| c.name == "cache::redis").unwrap();
        // Each module gets its own arg, no cross-contamination.
        assert_eq!(migrate.env.get("DB").unwrap(), "pg://db");
        assert_eq!(redis.env.get("REDIS").unwrap(), "redis://cache");
        assert!(!migrate.env.contains_key("REDIS"));
        assert!(!redis.env.contains_key("DB"));
    }

    #[test]
    fn lower_import_multiple_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            arg pool_size { type = string }
            job migrate {
                env { DB = args.url POOL = args.pool_size }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "pg://localhost" pool_size = "10" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "pg://localhost");
        assert_eq!(migrate.env.get("POOL").unwrap(), "10");
    }

    #[test]
    fn lower_default_used_when_no_binding() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string default = "pg://default" }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "pg://default");
    }

    #[test]
    fn lower_cli_override_beats_binding() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate {
                env { DB = args.url }
                run "migrate"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "from-binding" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let mut arg_values = HashMap::new();
        arg_values.insert("db::url".to_string(), "from-cli".to_string());
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &arg_values).unwrap();
        let migrate = configs.iter().find(|c| c.name == "db::migrate").unwrap();
        assert_eq!(migrate.env.get("DB").unwrap(), "from-cli");
    }

    #[test]
    fn lower_nested_import_entities() {
        let dir = tempfile::tempdir().unwrap();
        let inner_path = dir.path().join("inner.pman");
        std::fs::write(&inner_path, r#"job setup { run "setup" }"#).unwrap();

        let lib_path = dir.path().join("lib.pman");
        std::fs::write(
            &lib_path,
            r#"
            import "inner.pman" as inner
            job init {
                wait { after @inner::setup }
                run "init"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib
            service api {
                wait { after @lib::init }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        // Verify compound-prefixed runtime names.
        assert!(names.contains(&"lib::inner::setup"), "names: {names:?}");
        assert!(names.contains(&"lib::init"), "names: {names:?}");
        assert!(names.contains(&"api"), "names: {names:?}");
        // Verify lib::init depends on lib::inner::setup.
        let init = configs.iter().find(|c| c.name == "lib::init").unwrap();
        match &init.depends[0] {
            Dependency::ProcessExited { name, .. } => {
                assert_eq!(name, "lib::inner::setup");
            }
            other => panic!("expected ProcessExited, got {other:?}"),
        }
    }

    #[test]
    fn lower_nested_import_arg_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let inner_path = dir.path().join("inner.pman");
        std::fs::write(
            &inner_path,
            r#"
            arg port { type = string }
            job setup {
                env { PORT = args.port }
                run "setup"
            }
            "#,
        )
        .unwrap();

        let lib_path = dir.path().join("lib.pman");
        std::fs::write(
            &lib_path,
            r#"
            arg base_port { type = string }
            import "inner.pman" as inner { port = args.base_port }
            job init { run "init" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib { base_port = "9090" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();
        let setup = configs
            .iter()
            .find(|c| c.name == "lib::inner::setup")
            .unwrap();
        assert_eq!(setup.env.get("PORT").unwrap(), "9090");
    }

    #[test]
    fn nested_import_transitive_namespace_rejected() {
        // Root tries to reference @inner::setup, but inner is lib's private import.
        let dir = tempfile::tempdir().unwrap();
        let inner_path = dir.path().join("inner.pman");
        std::fs::write(&inner_path, r#"job setup { run "setup" }"#).unwrap();

        let lib_path = dir.path().join("lib.pman");
        std::fs::write(
            &lib_path,
            r#"
            import "inner.pman" as inner
            job init { run "init" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib
            service api {
                wait { after @inner::setup }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let err = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap_err();
        assert!(
            err.to_string().contains("unknown import alias"),
            "got: {err}"
        );
    }

    #[test]
    fn lower_task_sets_flags() {
        let (configs, _) = lower_str(r#"task test_a { run "echo test" }"#);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "test_a");
        assert!(!configs[0].autostart);
        assert!(configs[0].once);
        assert!(configs[0].is_task);
    }

    #[test]
    fn lower_module_dir_in_root() {
        let (configs, _) = lower_str(
            r#"
            job web {
                env DIR = module.dir
                run "serve"
            }
            "#,
        );
        let expected = std::fs::canonicalize(".").unwrap();
        assert_eq!(
            configs[0].env.get("DIR").unwrap(),
            expected.to_str().unwrap()
        );
    }

    #[test]
    fn lower_procman_dir_in_root() {
        let (configs, _) = lower_str(
            r#"
            job web {
                env DIR = procman.dir
                run "serve"
            }
            "#,
        );
        let expected = std::fs::canonicalize(".").unwrap();
        assert_eq!(
            configs[0].env.get("DIR").unwrap(),
            expected.to_str().unwrap()
        );
    }

    #[test]
    fn lower_module_dir_in_import() {
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(
            sub_dir.join("lib.pman"),
            r#"
            job setup {
                env MY_DIR = module.dir
                env ROOT_DIR = procman.dir
                run "echo dirs"
            }
            "#,
        )
        .unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "sub/lib.pman" as lib
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();

        let lib_setup = configs.iter().find(|c| c.name == "lib::setup").unwrap();
        let my_dir = lib_setup.env.get("MY_DIR").unwrap();
        let root_dir = lib_setup.env.get("ROOT_DIR").unwrap();

        // module.dir should point to the sub directory
        assert!(
            my_dir.ends_with("/sub"),
            "expected module.dir to end with /sub, got: {my_dir}"
        );
        // procman.dir should point to the root directory (canonicalized)
        let expected_root = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            root_dir,
            expected_root.to_str().unwrap(),
            "expected procman.dir to be root dir"
        );
    }

    #[test]
    fn lower_namespaced_module_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("sub");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(sub_dir.join("lib.pman"), r#"job setup { run "echo ok" }"#).unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "sub/lib.pman" as lib
            job web {
                env LIB_DIR = lib::module.dir
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();

        let web = configs.iter().find(|c| c.name == "web").unwrap();
        let lib_dir = web.env.get("LIB_DIR").unwrap();
        assert!(
            lib_dir.ends_with("/sub"),
            "expected lib::module.dir to end with /sub, got: {lib_dir}"
        );
    }

    #[test]
    fn lower_concat_strings() {
        let (configs, _) = lower_str(
            r#"
            job web {
                env X = "hello" + " world"
                run "echo"
            }
            "#,
        );
        assert_eq!(configs[0].env.get("X").unwrap(), "hello world");
    }

    #[test]
    fn lower_concat_with_args_ref() {
        let (configs, _) = lower_with_args(
            r#"
            arg base { type = string default = "/opt" }
            job web {
                env X = args.base + "/sub"
                run "echo"
            }
            "#,
            &[("base", "/opt")],
        );
        assert_eq!(configs[0].env.get("X").unwrap(), "/opt/sub");
    }

    /// Simulate the full pipeline: parse root, collect arg defs with defaults,
    /// fill defaults into arg_values, then lower.
    fn lower_full_pipeline(root_path: &std::path::Path) -> (Vec<ProcessConfig>, Option<String>) {
        let content = std::fs::read_to_string(root_path).unwrap();
        let path_str = root_path.to_str().unwrap();
        let root = crate::pman::parser::parse(&content, path_str).unwrap();
        let root_arg_defs = crate::pman::collect_root_arg_defs(&root, path_str).unwrap();
        let mut arg_values = HashMap::new();
        for def in &root_arg_defs {
            if let Some(ref default) = def.default {
                arg_values.insert(def.name.clone(), default.clone());
            }
        }
        let modules =
            crate::pman::loader::load_with_root(root, path_str, &arg_values, false).unwrap();
        let (configs, log_dir, _) = lower_modules(&modules, &HashMap::new(), &arg_values).unwrap();
        (configs, log_dir)
    }

    #[test]
    fn lower_procman_dir_in_arg_default() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            arg working_dir { type = string default = procman.dir + "/wd" }
            job web {
                env WD = args.working_dir
                run "echo"
            }
            "#,
        )
        .unwrap();

        let (configs, _) = lower_full_pipeline(&root_path);
        let wd = configs[0].env.get("WD").unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(wd, &format!("{canonical_dir}/wd"));
    }

    #[test]
    fn lower_module_dir_in_arg_default() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(
            sub.join("mod.pman"),
            r#"
            arg data_dir { type = string default = module.dir + "/data" }
            job worker {
                env DATA = args.data_dir
                run "echo"
            }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "sub/mod.pman" as sub
            job web { run "echo" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &HashMap::new()).unwrap();

        let worker = configs.iter().find(|c| c.name == "sub::worker").unwrap();
        let data = worker.env.get("DATA").unwrap();
        let canonical_sub = std::fs::canonicalize(&sub)
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(data, &format!("{canonical_sub}/data"));
    }

    #[test]
    fn lower_arg_default_references_other_arg() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            arg base { type = string default = "/opt" }
            arg sub_dir { type = string default = args.base + "/sub" }
            job web {
                env X = args.sub_dir
                run "echo"
            }
            "#,
        )
        .unwrap();

        let (configs, _) = lower_full_pipeline(&root_path);
        assert_eq!(configs[0].env.get("X").unwrap(), "/opt/sub");
    }

    #[test]
    fn lower_arg_default_cycle_detected() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            arg a { type = string default = args.b }
            arg b { type = string default = args.a }
            job web { run "echo" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let path_str = root_path.to_str().unwrap();
        let root = crate::pman::parser::parse(&content, path_str).unwrap();
        let result = crate::pman::collect_root_arg_defs(&root, path_str);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("cyclical"),
            "expected cyclical error, got: {err}"
        );
    }

    #[test]
    fn lower_arg_default_cli_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            arg base { type = string default = procman.dir + "/default" }
            job web {
                env X = args.base
                run "echo"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        let mut arg_values = HashMap::new();
        arg_values.insert("base".to_string(), "/override".to_string());
        let (configs, _, _) = lower_modules(&modules, &HashMap::new(), &arg_values).unwrap();

        assert_eq!(configs[0].env.get("X").unwrap(), "/override");
    }

    #[test]
    fn eval_string_lit_plain_string_unchanged() {
        let (configs, _) = lower_str(
            r#"
            job api {
                wait { connect "127.0.0.1:5432" }
                run "start"
            }
        "#,
        );
        match &configs[0].depends[0] {
            Dependency::TcpConnect { address, .. } => assert_eq!(address, "127.0.0.1:5432"),
            other => panic!("expected TcpConnect, got {other:?}"),
        }
    }

    #[test]
    fn eval_string_lit_args_in_connect() {
        let (configs, _) = lower_with_args(
            r#"
            arg host { type = string default = "localhost" }
            arg port { type = string default = "5432" }
            job api {
                wait { connect "${args.host}:${args.port}" }
                run "start"
            }
        "#,
            &[("host", "db.example.com"), ("port", "3306")],
        );
        match &configs[0].depends[0] {
            Dependency::TcpConnect { address, .. } => {
                assert_eq!(address, "db.example.com:3306");
            }
            other => panic!("expected TcpConnect, got {other:?}"),
        }
    }

    #[test]
    fn eval_string_lit_args_in_exists() {
        let (configs, _) = lower_with_args(
            r#"
            arg dir { type = string default = "/tmp" }
            job api {
                wait { exists "${args.dir}/config.yaml" }
                run "start"
            }
        "#,
            &[("dir", "/var/data")],
        );
        match &configs[0].depends[0] {
            Dependency::FileExists { path, .. } => assert_eq!(path, "/var/data/config.yaml"),
            other => panic!("expected FileExists, got {other:?}"),
        }
    }

    #[test]
    fn eval_string_lit_args_in_http() {
        let (configs, _) = lower_with_args(
            r#"
            arg port { type = string default = "8080" }
            job api {
                wait { http "http://localhost:${args.port}/health" }
                run "start"
            }
        "#,
            &[("port", "9090")],
        );
        match &configs[0].depends[0] {
            Dependency::HttpHealthCheck { url, .. } => {
                assert_eq!(url, "http://localhost:9090/health");
            }
            other => panic!("expected HttpHealthCheck, got {other:?}"),
        }
    }

    #[test]
    fn eval_string_lit_unterminated_ref_errors() {
        let arg_values = HashMap::from([("port".to_string(), "5432".to_string())]);
        let input = r#"
            arg port { type = string default = "8080" }
            job api {
                wait { connect "${args.port" }
                run "start"
            }
        "#;
        let err = lower(input, "test.pman", &HashMap::new(), &arg_values).unwrap_err();
        assert!(
            format!("{err}").contains("unterminated"),
            "expected 'unterminated' in error, got: {err}"
        );
    }

    #[test]
    fn eval_string_lit_unknown_ref_errors() {
        let arg_values = HashMap::from([("port".to_string(), "5432".to_string())]);
        let input = r#"
            arg port { type = string default = "8080" }
            job api {
                wait { connect "${bogus}" }
                run "start"
            }
        "#;
        let err = lower(input, "test.pman", &HashMap::new(), &arg_values).unwrap_err();
        assert!(
            format!("{err}").contains("unknown reference"),
            "expected 'unknown reference' in error, got: {err}"
        );
    }

    #[test]
    fn eval_string_lit_args_with_spaces() {
        let (configs, _) = lower_with_args(
            r#"
            arg dir { type = string default = "/tmp" }
            job api {
                wait { exists "${ args.dir }/config.yaml" }
                run "start"
            }
        "#,
            &[("dir", "/var/data")],
        );
        match &configs[0].depends[0] {
            Dependency::FileExists { path, .. } => assert_eq!(path, "/var/data/config.yaml"),
            other => panic!("expected FileExists, got {other:?}"),
        }
    }

    #[test]
    fn eval_string_lit_module_dir() {
        let dir = tempfile::tempdir().unwrap();
        let pman_path = dir.path().join("test.pman");
        let input = r#"
            job api {
                wait { exists "${module.dir}/config.yaml" }
                run "start"
            }
        "#;
        std::fs::write(&pman_path, input).unwrap();
        let canonical_dir = std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        let (configs, _) = lower(
            input,
            pman_path.to_str().unwrap(),
            &HashMap::new(),
            &HashMap::new(),
        )
        .unwrap();
        match &configs[0].depends[0] {
            Dependency::FileExists { path, .. } => {
                assert_eq!(path, &format!("{canonical_dir}/config.yaml"));
            }
            other => panic!("expected FileExists, got {other:?}"),
        }
    }
}
