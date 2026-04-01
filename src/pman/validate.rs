use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use crate::pman::{
    ast::{self, Expr, RunSection, ShellBlock},
    loader::LoadedModules,
    token::Span,
};

pub fn validate(file: &ast::File, path: &str) -> Result<()> {
    let mut arg_names: HashSet<&str> = HashSet::new();
    let mut job_names: HashSet<&str> = HashSet::new();
    let mut service_names: HashSet<&str> = HashSet::new();
    let mut event_names: HashSet<&str> = HashSet::new();
    // Jobs are once=true by default; collect all job names as once jobs.
    let mut once_jobs: HashSet<&str> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();

    // Step 0a: Check duplicate arg names.
    for arg in &file.args {
        if !arg_names.insert(&arg.name) {
            errors.push(
                arg.span
                    .fmt_error(path, &format!("duplicate arg name '{}'", arg.name)),
            );
        }
    }

    // Step 0b: Check duplicate env keys.
    let mut seen_env_keys: HashSet<&str> = HashSet::new();
    for binding in &file.env {
        if !seen_env_keys.insert(&binding.key) {
            errors.push(
                binding
                    .span
                    .fmt_error(path, &format!("duplicate env key '{}'", binding.key)),
            );
        }
    }

    // Step 1: Collect names and check duplicates.
    for job in &file.jobs {
        if !job_names.insert(&job.name) {
            errors.push(
                job.span
                    .fmt_error(path, &format!("duplicate job name '{}'", job.name)),
            );
        }
        // Jobs default to once=true.
        once_jobs.insert(&job.name);
    }
    for service in &file.services {
        if !service_names.insert(&service.name) {
            errors.push(
                service
                    .span
                    .fmt_error(path, &format!("duplicate service name '{}'", service.name)),
            );
        }
        // Also register service names in job_names so after-references can find them.
        if !job_names.insert(&service.name) {
            errors.push(
                service
                    .span
                    .fmt_error(path, &format!("duplicate name '{}'", service.name)),
            );
        }
    }
    for event in &file.events {
        if !event_names.insert(&event.name) {
            errors.push(
                event
                    .span
                    .fmt_error(path, &format!("duplicate event name '{}'", event.name)),
            );
        }
    }
    for task in &file.tasks {
        // Tasks are once=true; register in once_jobs and job_names (like jobs).
        once_jobs.insert(&task.name);
        if !job_names.insert(&task.name) {
            errors.push(
                task.span
                    .fmt_error(path, &format!("duplicate name '{}'", task.name)),
            );
        }
    }
    for name in &job_names {
        if event_names.contains(name) {
            let event = file.events.iter().find(|e| e.name == *name).unwrap();
            errors.push(event.span.fmt_error(
                path,
                &format!("name '{}' is used as both a job and an event", name),
            ));
        }
    }

    // Early return if names are invalid — later steps depend on correct names.
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    // Build after-edges for cycle detection and reachability.
    let mut after_edges: HashMap<&str, HashSet<&str>> = HashMap::new();

    // Step 2: Validate after references + build edges.
    // Namespaced refs (e.g. @ns::job) are deferred to cross-module validation.
    for job in &file.jobs {
        let mut targets = HashSet::new();
        if let Some(wait) = &job.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    if namespace.is_some() {
                        continue; // deferred to validate_cross_refs
                    }
                    if !job_names.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "job '{}': after @{} references unknown job",
                                job.name, target
                            ),
                        ));
                    } else if !once_jobs.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!("job '{}': after @{} target must be a job", job.name, target),
                        ));
                    }
                    targets.insert(target.as_str());
                }
            }
        }
        after_edges.insert(job.name.as_str(), targets);
    }
    for service in &file.services {
        let mut targets = HashSet::new();
        if let Some(wait) = &service.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    if namespace.is_some() {
                        continue;
                    }
                    if !job_names.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "service '{}': after @{} references unknown job",
                                service.name, target
                            ),
                        ));
                    } else if !once_jobs.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "service '{}': after @{} target must be a job",
                                service.name, target
                            ),
                        ));
                    }
                    targets.insert(target.as_str());
                }
            }
        }
        after_edges.insert(service.name.as_str(), targets);
    }
    for event in &file.events {
        let mut targets = HashSet::new();
        if let Some(wait) = &event.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    if namespace.is_some() {
                        continue;
                    }
                    if !job_names.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "event '{}': after @{} references unknown job",
                                event.name, target
                            ),
                        ));
                    } else if !once_jobs.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "event '{}': after @{} target must be a job",
                                event.name, target
                            ),
                        ));
                    }
                    targets.insert(target.as_str());
                }
            }
        }
        after_edges.insert(event.name.as_str(), targets);
    }
    for task in &file.tasks {
        let mut targets = HashSet::new();
        if let Some(wait) = &task.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    if namespace.is_some() {
                        continue;
                    }
                    if !job_names.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "task '{}': after @{} references unknown job",
                                task.name, target
                            ),
                        ));
                    } else if !once_jobs.contains(target.as_str()) {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "task '{}': after @{} target must be a job",
                                task.name, target
                            ),
                        ));
                    }
                    targets.insert(target.as_str());
                }
            }
        }
        after_edges.insert(task.name.as_str(), targets);
    }

    // Early return if after-references are invalid.
    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    // Step 3: Cycle detection — fail-fast since reachability depends on acyclic graph.
    detect_cycles(&after_edges)?;

    // Step 4: Validate @job.KEY output references.
    for job in &file.jobs {
        errors.extend(validate_output_refs(
            &job.name,
            &job.body,
            &once_jobs,
            &after_edges,
            path,
        ));
    }
    for service in &file.services {
        errors.extend(validate_output_refs(
            &service.name,
            &service.body,
            &once_jobs,
            &after_edges,
            path,
        ));
    }
    for event in &file.events {
        errors.extend(validate_output_refs(
            &event.name,
            &event.body,
            &once_jobs,
            &after_edges,
            path,
        ));
    }
    for task in &file.tasks {
        errors.extend(validate_output_refs(
            &task.name,
            &task.body,
            &once_jobs,
            &after_edges,
            path,
        ));
    }

    // Step 5: Validate on_fail spawn references.
    for job in &file.jobs {
        errors.extend(validate_spawns(
            &job.body,
            &job_names,
            &service_names,
            &event_names,
            path,
        ));
    }
    for service in &file.services {
        errors.extend(validate_spawns(
            &service.body,
            &job_names,
            &service_names,
            &event_names,
            path,
        ));
    }
    for event in &file.events {
        errors.extend(validate_spawns(
            &event.body,
            &job_names,
            &service_names,
            &event_names,
            path,
        ));
    }
    for task in &file.tasks {
        errors.extend(validate_spawns(
            &task.body,
            &job_names,
            &service_names,
            &event_names,
            path,
        ));
    }

    // Step 6: Variable shadowing.
    for job in &file.jobs {
        errors.extend(check_variable_shadowing(&job.body, path));
    }
    for service in &file.services {
        errors.extend(check_variable_shadowing(&service.body, path));
    }
    for event in &file.events {
        errors.extend(check_variable_shadowing(&event.body, path));
    }
    for task in &file.tasks {
        errors.extend(check_variable_shadowing(&task.body, path));
    }

    // Step 7: Duplicate watch names.
    for job in &file.jobs {
        errors.extend(check_duplicate_watches(&job.body, path));
    }
    for service in &file.services {
        errors.extend(check_duplicate_watches(&service.body, path));
    }
    for event in &file.events {
        errors.extend(check_duplicate_watches(&event.body, path));
    }
    for task in &file.tasks {
        errors.extend(check_duplicate_watches(&task.body, path));
    }

    // Step 8: Empty run rejection.
    for job in &file.jobs {
        errors.extend(check_empty_run(&job.body, path));
    }
    for service in &file.services {
        errors.extend(check_empty_run(&service.body, path));
    }
    for event in &file.events {
        errors.extend(check_empty_run(&event.body, path));
    }
    for task in &file.tasks {
        errors.extend(check_empty_run(&task.body, path));
    }

    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    Ok(())
}

fn collect_output_refs(expr: &Expr) -> Vec<(&str, &str, Span)> {
    match expr {
        // Skip namespaced refs — deferred to cross-module validation.
        Expr::JobOutputRef(Some(_), _, _, _) => vec![],
        Expr::JobOutputRef(None, job, key, span) => vec![(job.as_str(), key.as_str(), *span)],
        Expr::BinOp(lhs, _, rhs, _) => {
            let mut refs = collect_output_refs(lhs);
            refs.extend(collect_output_refs(rhs));
            refs
        }
        Expr::UnaryNot(inner, _) => collect_output_refs(inner),
        _ => vec![],
    }
}

fn is_reachable(from: &str, to: &str, edges: &HashMap<&str, HashSet<&str>>) -> bool {
    let mut visited = HashSet::new();
    let mut stack = vec![from];
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if visited.insert(node)
            && let Some(neighbors) = edges.get(node)
        {
            for &neighbor in neighbors {
                stack.push(neighbor);
            }
        }
    }
    false
}

fn validate_output_refs(
    owner_name: &str,
    body: &ast::JobBody,
    once_jobs: &HashSet<&str>,
    after_edges: &HashMap<&str, HashSet<&str>>,
    path: &str,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut all_refs = Vec::new();
    for env in &body.env {
        all_refs.extend(collect_output_refs(&env.value));
    }
    if let RunSection::ForLoop(fl) = &body.run_section {
        for env in &fl.env {
            all_refs.extend(collect_output_refs(&env.value));
        }
    }
    for (job, _key, span) in all_refs {
        if !once_jobs.contains(job) {
            errors.push(span.fmt_error(
                path,
                &format!(
                    "'{}': @{}.* reference requires '{}' to be a job",
                    owner_name, job, job
                ),
            ));
        } else if !is_reachable(owner_name, job, after_edges) {
            errors.push(span.fmt_error(
                path,
                &format!(
                    "'{}': @{}.* reference requires after @{} (direct or transitive)",
                    owner_name, job, job
                ),
            ));
        }
    }
    errors
}

fn detect_cycles(edges: &HashMap<&str, HashSet<&str>>) -> Result<()> {
    let all_nodes: HashSet<&str> = edges.keys().copied().collect();
    let mut color: HashMap<&str, u8> = all_nodes.iter().map(|&n| (n, 0u8)).collect();
    let mut path: Vec<&str> = Vec::new();

    // Convert HashSet edges to Vec for the DFS helper.
    let vec_edges: HashMap<&str, Vec<&str>> = edges
        .iter()
        .map(|(&k, v)| (k, v.iter().copied().collect()))
        .collect();

    for &start in &all_nodes {
        if color[start] == 0
            && let Some(cycle) = dfs_find_cycle(start, &vec_edges, &mut color, &mut path)
        {
            bail!("circular dependency: {}", cycle.join(" -> "));
        }
    }
    Ok(())
}

fn dfs_find_cycle<'a>(
    node: &'a str,
    edges: &HashMap<&'a str, Vec<&'a str>>,
    color: &mut HashMap<&'a str, u8>,
    path: &mut Vec<&'a str>,
) -> Option<Vec<String>> {
    color.insert(node, 1);
    path.push(node);

    if let Some(neighbors) = edges.get(node) {
        for &neighbor in neighbors {
            match color.get(neighbor).copied().unwrap_or(0) {
                1 => {
                    let start = path.iter().position(|&n| n == neighbor).unwrap();
                    let mut cycle: Vec<String> =
                        path[start..].iter().map(|s| s.to_string()).collect();
                    cycle.push(neighbor.to_string());
                    return Some(cycle);
                }
                0 => {
                    if let Some(cycle) = dfs_find_cycle(neighbor, edges, color, path) {
                        return Some(cycle);
                    }
                }
                _ => {}
            }
        }
    }

    color.insert(node, 2);
    path.pop();
    None
}

fn validate_spawns(
    body: &ast::JobBody,
    job_names: &HashSet<&str>,
    service_names: &HashSet<&str>,
    event_names: &HashSet<&str>,
    path: &str,
) -> Vec<String> {
    let mut errors = Vec::new();
    for watch in &body.watches {
        if let Some(ast::OnFailAction::Spawn(ns, target)) = &watch.on_fail {
            // Skip namespaced refs — deferred to cross-module validation.
            if ns.is_some() {
                continue;
            }
            let t = target.as_str();
            if event_names.contains(t) {
                // OK — events are valid spawn targets
            } else if job_names.contains(t) {
                errors.push(watch.span.fmt_error(
                    path,
                    &format!("on_fail spawn @{target} must reference an event, not a job"),
                ));
            } else if service_names.contains(t) {
                errors.push(watch.span.fmt_error(
                    path,
                    &format!("on_fail spawn @{target} must reference an event, not a service"),
                ));
            } else {
                errors.push(watch.span.fmt_error(
                    path,
                    &format!("on_fail spawn @{target} references unknown target"),
                ));
            }
        }
    }
    errors
}

fn check_variable_shadowing(body: &ast::JobBody, path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut vars: HashSet<&str> = HashSet::new();
    if let Some(wait) = &body.wait {
        for cond in &wait.conditions {
            if let ast::ConditionKind::Contains { var: Some(v), .. } = &cond.kind
                && !vars.insert(v.as_str())
            {
                errors.push(
                    cond.span
                        .fmt_error(path, &format!("variable '{}' shadows existing variable", v)),
                );
            }
        }
    }
    if let RunSection::ForLoop(fl) = &body.run_section
        && !vars.insert(fl.var.as_str())
    {
        errors.push(fl.span.fmt_error(
            path,
            &format!("for-loop variable '{}' shadows existing variable", fl.var),
        ));
    }
    errors
}

fn check_duplicate_watches(body: &ast::JobBody, path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut names: HashSet<&str> = HashSet::new();
    for watch in &body.watches {
        if !names.insert(&watch.name) {
            errors.push(
                watch
                    .span
                    .fmt_error(path, &format!("duplicate watch name '{}'", watch.name)),
            );
        }
    }
    errors
}

fn check_empty_run(body: &ast::JobBody, path: &str) -> Vec<String> {
    let mut errors = Vec::new();
    match &body.run_section {
        RunSection::Direct(ShellBlock::Inline(s)) if s.value.is_empty() => {
            errors.push(s.span.fmt_error(path, "run command must not be empty"));
        }
        RunSection::Direct(ShellBlock::Fenced(s, span)) if s.is_empty() => {
            errors.push(span.fmt_error(path, "run command must not be empty"));
        }
        RunSection::ForLoop(fl) => match &fl.run {
            ShellBlock::Inline(s) if s.value.is_empty() => {
                errors.push(s.span.fmt_error(path, "run command must not be empty"));
            }
            ShellBlock::Fenced(s, span) if s.is_empty() => {
                errors.push(span.fmt_error(path, "run command must not be empty"));
            }
            _ => {}
        },
        _ => {}
    }
    errors
}

struct ModuleEntities<'a> {
    jobs: HashSet<&'a str>,
    services: HashSet<&'a str>,
    events: HashSet<&'a str>,
    once_jobs: HashSet<&'a str>,
    args: HashSet<&'a str>,
}

fn build_module_entities(file: &ast::File) -> ModuleEntities<'_> {
    let mut entities = ModuleEntities {
        jobs: HashSet::new(),
        services: HashSet::new(),
        events: HashSet::new(),
        once_jobs: HashSet::new(),
        args: HashSet::new(),
    };
    for arg in &file.args {
        entities.args.insert(&arg.name);
    }
    for job in &file.jobs {
        entities.jobs.insert(&job.name);
        entities.once_jobs.insert(&job.name);
    }
    for service in &file.services {
        entities.services.insert(&service.name);
    }
    for event in &file.events {
        entities.events.insert(&event.name);
    }
    for task in &file.tasks {
        entities.jobs.insert(&task.name);
        entities.once_jobs.insert(&task.name);
    }
    entities
}

fn validate_namespaced_after_refs_in_body(
    owner_name: &str,
    body: &ast::JobBody,
    registry: &HashMap<&str, ModuleEntities<'_>>,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(wait) = &body.wait {
        for cond in &wait.conditions {
            if let ast::ConditionKind::After {
                namespace: Some(ns),
                job: target,
            } = &cond.kind
            {
                match registry.get(ns.as_str()) {
                    None => {
                        errors.push(cond.span.fmt_error(
                            path,
                            &format!(
                                "'{owner_name}': after @{ns}::{target} references unknown import alias '{ns}'"
                            ),
                        ));
                    }
                    Some(entities) => {
                        if !entities.jobs.contains(target.as_str())
                            && !entities.services.contains(target.as_str())
                        {
                            errors.push(cond.span.fmt_error(
                                path,
                                &format!(
                                    "'{owner_name}': after @{ns}::{target} references unknown entity in '{ns}'"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn validate_namespaced_output_refs_in_body(
    owner_name: &str,
    body: &ast::JobBody,
    registry: &HashMap<&str, ModuleEntities<'_>>,
    path: &str,
    errors: &mut Vec<String>,
) {
    let mut all_exprs: Vec<&Expr> = Vec::new();
    for env in &body.env {
        all_exprs.push(&env.value);
    }
    if let RunSection::ForLoop(fl) = &body.run_section {
        for env in &fl.env {
            all_exprs.push(&env.value);
        }
    }
    for expr in all_exprs {
        collect_namespaced_output_refs(expr, owner_name, registry, path, errors);
    }
}

fn collect_namespaced_output_refs(
    expr: &Expr,
    owner_name: &str,
    registry: &HashMap<&str, ModuleEntities<'_>>,
    path: &str,
    errors: &mut Vec<String>,
) {
    match expr {
        Expr::JobOutputRef(Some(ns), job, _key, span) => match registry.get(ns.as_str()) {
            None => {
                errors.push(span.fmt_error(
                    path,
                    &format!(
                        "'{owner_name}': @{ns}::{job}.* references unknown import alias '{ns}'"
                    ),
                ));
            }
            Some(entities) => {
                if !entities.once_jobs.contains(job.as_str()) {
                    errors.push(span.fmt_error(
                        path,
                        &format!(
                            "'{owner_name}': @{ns}::{job}.* requires '{job}' to be a job in '{ns}'"
                        ),
                    ));
                }
            }
        },
        Expr::BinOp(lhs, _, rhs, _) => {
            collect_namespaced_output_refs(lhs, owner_name, registry, path, errors);
            collect_namespaced_output_refs(rhs, owner_name, registry, path, errors);
        }
        Expr::NamespacedArgsRef(ns, name, span) => match registry.get(ns.as_str()) {
            None => {
                errors.push(span.fmt_error(
                    path,
                    &format!(
                        "'{owner_name}': {ns}::args.{name} references unknown import alias '{ns}'"
                    ),
                ));
            }
            Some(entities) => {
                if !entities.args.contains(name.as_str()) {
                    errors.push(span.fmt_error(
                        path,
                        &format!(
                            "'{owner_name}': {ns}::args.{name} references unknown arg '{name}' in '{ns}'"
                        ),
                    ));
                }
            }
        },
        Expr::UnaryNot(inner, _) => {
            collect_namespaced_output_refs(inner, owner_name, registry, path, errors);
        }
        _ => {}
    }
}

fn validate_namespaced_spawns_in_body(
    body: &ast::JobBody,
    registry: &HashMap<&str, ModuleEntities<'_>>,
    path: &str,
    errors: &mut Vec<String>,
) {
    for watch in &body.watches {
        if let Some(ast::OnFailAction::Spawn(Some(ns), target)) = &watch.on_fail {
            match registry.get(ns.as_str()) {
                None => {
                    errors.push(watch.span.fmt_error(
                        path,
                        &format!(
                            "on_fail spawn @{ns}::{target} references unknown import alias '{ns}'"
                        ),
                    ));
                }
                Some(entities) => {
                    if !entities.events.contains(target.as_str()) {
                        errors.push(watch.span.fmt_error(
                            path,
                            &format!(
                                "on_fail spawn @{ns}::{target} must reference an event in '{ns}'"
                            ),
                        ));
                    }
                }
            }
        }
    }
}

/// Validate one file's cross-module refs against its direct imports' registry.
fn validate_module_cross_refs(
    file: &ast::File,
    path: &str,
    direct_imports: &HashMap<String, crate::pman::loader::LoadedModule>,
    errors: &mut Vec<String>,
) {
    // Build a registry of each direct import's entities.
    let mut registry: HashMap<&str, ModuleEntities<'_>> = HashMap::new();
    for (alias, module) in direct_imports {
        registry.insert(alias.as_str(), build_module_entities(&module.file));
    }

    // Validate import binding names reference existing args in target modules.
    for import_def in &file.imports {
        if let Some(entities) = registry.get(import_def.alias.as_str()) {
            for binding in &import_def.bindings {
                if !entities.args.contains(binding.name.as_str()) {
                    errors.push(binding.span.fmt_error(
                        path,
                        &format!(
                            "import '{}': binding '{}' does not match any arg in the imported module",
                            import_def.alias, binding.name,
                        ),
                    ));
                }
            }
        }
    }

    // Validate namespaced refs in this file's entities.
    for job in &file.jobs {
        validate_namespaced_after_refs_in_body(&job.name, &job.body, &registry, path, errors);
        validate_namespaced_output_refs_in_body(&job.name, &job.body, &registry, path, errors);
        validate_namespaced_spawns_in_body(&job.body, &registry, path, errors);
    }
    for service in &file.services {
        validate_namespaced_after_refs_in_body(
            &service.name,
            &service.body,
            &registry,
            path,
            errors,
        );
        validate_namespaced_output_refs_in_body(
            &service.name,
            &service.body,
            &registry,
            path,
            errors,
        );
        validate_namespaced_spawns_in_body(&service.body, &registry, path, errors);
    }
    for event in &file.events {
        validate_namespaced_after_refs_in_body(&event.name, &event.body, &registry, path, errors);
        validate_namespaced_output_refs_in_body(&event.name, &event.body, &registry, path, errors);
        validate_namespaced_spawns_in_body(&event.body, &registry, path, errors);
    }
    for task in &file.tasks {
        validate_namespaced_after_refs_in_body(&task.name, &task.body, &registry, path, errors);
        validate_namespaced_output_refs_in_body(&task.name, &task.body, &registry, path, errors);
        validate_namespaced_spawns_in_body(&task.body, &registry, path, errors);
    }

    // Recurse into sub-imports.
    for module in direct_imports.values() {
        validate_module_cross_refs(&module.file, &module.path, &module.imports, errors);
    }
}

/// Build after-edges for one file's entities, using `prefix` for qualified names.
/// A namespaced ref `@ns::target` at prefix `P` becomes `{P}::{ns}::{target}`.
/// An unnamespaced ref `@target` at prefix `P` becomes `{P}::{target}` (or just `target` if no prefix).
fn build_edges_for_file(
    file: &ast::File,
    prefix: Option<&str>,
    edges: &mut HashMap<String, HashSet<String>>,
) {
    for job in &file.jobs {
        let qualified = match prefix {
            Some(p) => format!("{p}::{}", job.name),
            None => job.name.clone(),
        };
        let mut targets = HashSet::new();
        if let Some(wait) = &job.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    targets.insert(match (prefix, namespace.as_deref()) {
                        (Some(p), Some(ns)) => format!("{p}::{ns}::{target}"),
                        (None, Some(ns)) => format!("{ns}::{target}"),
                        (Some(p), None) => format!("{p}::{target}"),
                        (None, None) => target.clone(),
                    });
                }
            }
        }
        edges.insert(qualified, targets);
    }
    for service in &file.services {
        let qualified = match prefix {
            Some(p) => format!("{p}::{}", service.name),
            None => service.name.clone(),
        };
        let mut targets = HashSet::new();
        if let Some(wait) = &service.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    targets.insert(match (prefix, namespace.as_deref()) {
                        (Some(p), Some(ns)) => format!("{p}::{ns}::{target}"),
                        (None, Some(ns)) => format!("{ns}::{target}"),
                        (Some(p), None) => format!("{p}::{target}"),
                        (None, None) => target.clone(),
                    });
                }
            }
        }
        edges.insert(qualified, targets);
    }
    for event in &file.events {
        let qualified = match prefix {
            Some(p) => format!("{p}::{}", event.name),
            None => event.name.clone(),
        };
        let mut targets = HashSet::new();
        if let Some(wait) = &event.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    targets.insert(match (prefix, namespace.as_deref()) {
                        (Some(p), Some(ns)) => format!("{p}::{ns}::{target}"),
                        (None, Some(ns)) => format!("{ns}::{target}"),
                        (Some(p), None) => format!("{p}::{target}"),
                        (None, None) => target.clone(),
                    });
                }
            }
        }
        edges.insert(qualified, targets);
    }
    for task in &file.tasks {
        let qualified = match prefix {
            Some(p) => format!("{p}::{}", task.name),
            None => task.name.clone(),
        };
        let mut targets = HashSet::new();
        if let Some(wait) = &task.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After {
                    namespace,
                    job: target,
                } = &cond.kind
                {
                    targets.insert(match (prefix, namespace.as_deref()) {
                        (Some(p), Some(ns)) => format!("{p}::{ns}::{target}"),
                        (None, Some(ns)) => format!("{ns}::{target}"),
                        (Some(p), None) => format!("{p}::{target}"),
                        (None, None) => target.clone(),
                    });
                }
            }
        }
        edges.insert(qualified, targets);
    }
}

/// Recursively build edges for all modules in the tree.
fn build_edges_recursive(
    imports: &HashMap<String, crate::pman::loader::LoadedModule>,
    prefix: Option<&str>,
    edges: &mut HashMap<String, HashSet<String>>,
) {
    for (alias, module) in imports {
        let compound = match prefix {
            Some(p) => format!("{p}::{alias}"),
            None => alias.clone(),
        };
        build_edges_for_file(&module.file, Some(&compound), edges);
        build_edges_recursive(&module.imports, Some(&compound), edges);
    }
}

pub fn validate_cross_refs(modules: &LoadedModules) -> Result<()> {
    let mut errors = Vec::new();

    // Validate root file's cross-refs against its direct imports.
    validate_module_cross_refs(
        &modules.root,
        &modules.root_path,
        &modules.imports,
        &mut errors,
    );

    // Build combined after-edges graph with qualified names for cycle detection.
    let mut combined_edges: HashMap<String, HashSet<String>> = HashMap::new();

    // Root file edges.
    build_edges_for_file(&modules.root, None, &mut combined_edges);

    // Recursively build edges for all imported modules.
    build_edges_recursive(&modules.imports, None, &mut combined_edges);

    // Cycle detection on combined graph.
    let str_edges: HashMap<&str, HashSet<&str>> = combined_edges
        .iter()
        .map(|(k, v)| (k.as_str(), v.iter().map(|s| s.as_str()).collect()))
        .collect();
    detect_cycles(&str_edges)?;

    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pman::parser::parse;

    fn parse_and_validate(input: &str) -> Result<()> {
        let file = parse(input, "test.pman")?;
        validate(&file, "test.pman")
    }

    #[test]
    fn valid_simple_job() {
        parse_and_validate(r#"job web { run "serve" }"#).unwrap();
    }

    #[test]
    fn after_must_target_job() {
        let input = r#"
            service web { run "serve" }
            job worker {
                wait { after @web }
                run "work"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("must be a job"), "got: {}", err);
    }

    #[test]
    fn after_unknown_job_errors() {
        let input = r#"
            job worker {
                wait { after @nonexistent }
                run "work"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("unknown job"), "got: {}", err);
    }

    #[test]
    fn job_output_ref_requires_after() {
        let input = r#"
            job setup { run "setup" }
            job web {
                env { URL = @setup.URL }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("requires after"), "got: {}", err);
    }

    #[test]
    fn job_output_ref_requires_job_target() {
        let input = r#"
            service web { run "serve" }
            job worker {
                wait { after @web }
                env { URL = @web.URL }
                run "work"
            }
        "#;
        // web is a service (not a job), so after @web should fail.
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("must be a job"), "got: {}", err);
    }

    #[test]
    fn circular_dependency_detected() {
        let input = r#"
            job a { wait { after @b } run "a" }
            job b { wait { after @a } run "b" }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("circular dependency"),
            "got: {}",
            err
        );
    }

    #[test]
    fn spawn_must_target_event() {
        let input = r#"
            job web {
                watch health {
                    http "http://localhost:8080/health"
                    on_fail spawn @web
                }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("must reference an event"),
            "got: {}",
            err
        );
    }

    #[test]
    fn spawn_target_exists() {
        let input = r#"
            job web {
                watch health {
                    http "http://localhost:8080/health"
                    on_fail spawn @nonexistent
                }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("unknown target"), "got: {}", err);
    }

    #[test]
    fn var_shadowing_detected() {
        let input = r#"
            job web {
                wait {
                    contains "/tmp/config.json" {
                        format = "json"
                        key = "$.host"
                        var = host
                    }
                    contains "/tmp/config.json" {
                        format = "json"
                        key = "$.port"
                        var = host
                    }
                }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("shadows existing variable"),
            "got: {}",
            err
        );
    }

    #[test]
    fn empty_run_rejected() {
        let input = r#"job web { run "" }"#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "got: {}",
            err
        );
    }

    #[test]
    fn duplicate_watch_names_detected() {
        let input = r#"
            job web {
                watch health {
                    http "http://localhost:8080/health"
                }
                watch health {
                    http "http://localhost:8080/ready"
                }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("duplicate watch name"),
            "got: {}",
            err
        );
    }

    #[test]
    fn transitive_after_satisfies_output_ref() {
        let input = r#"
            job setup { run "setup" }
            job middle { wait { after @setup } run "middle" }
            job web {
                wait { after @middle }
                env { URL = @setup.URL }
                run "serve"
            }
        "#;
        parse_and_validate(input).unwrap();
    }

    #[test]
    fn error_includes_file_location() {
        let input = r#"job web { run "serve" }
job web { run "other" }"#;
        let err = parse_and_validate(input).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("test.pman:"),
            "expected file location in error: {msg}"
        );
        assert!(msg.contains("error: duplicate job name"), "got: {msg}");
    }

    #[test]
    fn duplicate_arg_name_detected() {
        let input = r#"
            arg port { type = string default = "3000" }
            arg port { type = string default = "8080" }
            job web { run "serve" }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("duplicate arg name"),
            "got: {}",
            err
        );
    }

    #[test]
    fn duplicate_env_key_detected() {
        let input = r#"
            env {
                K = "a"
                K = "b"
            }
            job web { run "serve" }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("duplicate env key"),
            "got: {}",
            err
        );
    }

    #[test]
    fn namespaced_after_ref_valid() {
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
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        for module in modules.imports.values() {
            validate(&module.file, &module.path).unwrap();
        }
        validate_cross_refs(&modules).unwrap();
    }

    #[test]
    fn namespaced_after_ref_unknown_alias() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            service api {
                wait { after @bogus::migrate }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        let err = validate_cross_refs(&modules).unwrap_err();
        assert!(
            err.to_string().contains("unknown import alias"),
            "got: {err}"
        );
    }

    #[test]
    fn namespaced_after_ref_unknown_entity() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                wait { after @db::nonexistent }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        let err = validate_cross_refs(&modules).unwrap_err();
        assert!(err.to_string().contains("unknown entity"), "got: {err}");
    }

    #[test]
    fn namespaced_spawn_ref_valid() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"event recovery { run "recover" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                watch health {
                    http "http://localhost:8080/health"
                    on_fail spawn @db::recovery
                }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        for module in modules.imports.values() {
            validate(&module.file, &module.path).unwrap();
        }
        validate_cross_refs(&modules).unwrap();
    }

    #[test]
    fn import_binding_unknown_arg_errors() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "x" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        let err = validate_cross_refs(&modules).unwrap_err();
        assert!(
            err.to_string().contains("does not match any arg"),
            "got: {err}"
        );
    }

    #[test]
    fn import_binding_valid_arg_ok() {
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
            import "db.pman" as db { url = "postgres://localhost/mydb" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        for module in modules.imports.values() {
            validate(&module.file, &module.path).unwrap();
        }
        validate_cross_refs(&modules).unwrap();
    }

    #[test]
    fn namespaced_args_ref_unknown_alias() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            service api {
                env { DB_URL = db::args.url }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        let err = validate_cross_refs(&modules).unwrap_err();
        assert!(
            err.to_string().contains("unknown import alias"),
            "got: {err}"
        );
    }

    #[test]
    fn namespaced_args_ref_unknown_arg() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            service api {
                env { DB_URL = db::args.url }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        let err = validate_cross_refs(&modules).unwrap_err();
        assert!(err.to_string().contains("unknown arg"), "got: {err}");
    }

    #[test]
    fn namespaced_args_ref_valid() {
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
            service api {
                env { DB_URL = db::args.url }
                run "serve"
            }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = crate::pman::loader::load(&content, root_path.to_str().unwrap()).unwrap();
        validate(&modules.root, root_path.to_str().unwrap()).unwrap();
        for module in modules.imports.values() {
            validate(&module.file, &module.path).unwrap();
        }
        validate_cross_refs(&modules).unwrap();
    }

    #[test]
    fn valid_simple_task() {
        parse_and_validate(r#"task test_a { run "echo test" }"#).unwrap();
    }

    #[test]
    fn duplicate_task_name_errors() {
        let err = parse_and_validate(
            r#"
            task t { run "a" }
            task t { run "b" }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate name"), "got: {err}");
    }

    #[test]
    fn task_name_collides_with_job() {
        let err = parse_and_validate(
            r#"
            job x { run "a" }
            task x { run "b" }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("duplicate name"), "got: {err}");
    }

    #[test]
    fn task_with_after_valid() {
        parse_and_validate(
            r#"
            job setup { run "setup" }
            task test_a {
                wait { after @setup }
                run "test"
            }
        "#,
        )
        .unwrap();
    }
}
