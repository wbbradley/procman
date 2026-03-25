use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use crate::pman::{
    ast::{self, Expr, RunSection, ShellBlock},
    token::Span,
};

pub fn validate(file: &ast::File, path: &str) -> Result<()> {
    let mut job_names: HashSet<&str> = HashSet::new();
    let mut event_names: HashSet<&str> = HashSet::new();
    let mut once_jobs: HashSet<&str> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();

    // Step 1: Collect names and check duplicates.
    for job in &file.jobs {
        if !job_names.insert(&job.name) {
            errors.push(
                job.span
                    .fmt_error(path, &format!("duplicate job name '{}'", job.name)),
            );
        }
        if job.body.once == Some(true) {
            once_jobs.insert(&job.name);
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
    for job in &file.jobs {
        let mut targets = HashSet::new();
        if let Some(wait) = &job.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After { job: target } = &cond.kind {
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
                            &format!(
                                "job '{}': after @{} target must be once = true",
                                job.name, target
                            ),
                        ));
                    }
                    targets.insert(target.as_str());
                }
            }
        }
        after_edges.insert(job.name.as_str(), targets);
    }
    for event in &file.events {
        let mut targets = HashSet::new();
        if let Some(wait) = &event.body.wait {
            for cond in &wait.conditions {
                if let ast::ConditionKind::After { job: target } = &cond.kind {
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
                                "event '{}': after @{} target must be once = true",
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
    for event in &file.events {
        errors.extend(validate_output_refs(
            &event.name,
            &event.body,
            &once_jobs,
            &after_edges,
            path,
        ));
    }

    // Step 5: Validate on_fail spawn references.
    for job in &file.jobs {
        errors.extend(validate_spawns(&job.body, &job_names, &event_names, path));
    }
    for event in &file.events {
        errors.extend(validate_spawns(&event.body, &job_names, &event_names, path));
    }

    // Step 6: Variable shadowing.
    for job in &file.jobs {
        errors.extend(check_variable_shadowing(&job.body, path));
    }
    for event in &file.events {
        errors.extend(check_variable_shadowing(&event.body, path));
    }

    // Step 7: Duplicate watch names.
    for job in &file.jobs {
        errors.extend(check_duplicate_watches(&job.body, path));
    }
    for event in &file.events {
        errors.extend(check_duplicate_watches(&event.body, path));
    }

    // Step 8: Empty run rejection.
    for job in &file.jobs {
        errors.extend(check_empty_run(&job.body, path));
    }
    for event in &file.events {
        errors.extend(check_empty_run(&event.body, path));
    }

    if !errors.is_empty() {
        bail!("{}", errors.join("\n"));
    }

    Ok(())
}

fn collect_output_refs(expr: &Expr) -> Vec<(&str, &str, Span)> {
    match expr {
        Expr::JobOutputRef(job, key, span) => vec![(job.as_str(), key.as_str(), *span)],
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
                    "'{}': @{}.* reference requires '{}' to be once = true",
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
    event_names: &HashSet<&str>,
    path: &str,
) -> Vec<String> {
    let mut errors = Vec::new();
    for watch in &body.watches {
        if let Some(ast::OnFailAction::Spawn(target)) = &watch.on_fail {
            if !job_names.contains(target.as_str()) && !event_names.contains(target.as_str()) {
                errors.push(watch.span.fmt_error(
                    path,
                    &format!("on_fail spawn @{} references unknown target", target),
                ));
            } else if job_names.contains(target.as_str()) {
                errors.push(watch.span.fmt_error(
                    path,
                    &format!(
                        "on_fail spawn @{} must reference an event, not a job",
                        target
                    ),
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
    fn after_must_target_once_job() {
        let input = r#"
            job web { run "serve" }
            job worker {
                wait { after @web }
                run "work"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(
            err.to_string().contains("must be once = true"),
            "got: {}",
            err
        );
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
            job setup { once = true run "setup" }
            job web {
                env { URL = @setup.URL }
                run "serve"
            }
        "#;
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("requires after"), "got: {}", err);
    }

    #[test]
    fn job_output_ref_requires_once() {
        let input = r#"
            job web { run "serve" }
            job worker {
                wait { after @web }
                env { URL = @web.URL }
                run "work"
            }
        "#;
        // web is not once = true, so after @web itself should fail first.
        let err = parse_and_validate(input).unwrap_err();
        assert!(err.to_string().contains("once = true"), "got: {}", err);
    }

    #[test]
    fn circular_dependency_detected() {
        let input = r#"
            job a { once = true wait { after @b } run "a" }
            job b { once = true wait { after @a } run "b" }
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
            job setup { once = true run "setup" }
            job middle { once = true wait { after @setup } run "middle" }
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
        assert!(msg.contains("duplicate job name"), "got: {msg}");
    }
}
