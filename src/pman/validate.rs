use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

use crate::pman::ast::{self, Expr, RunSection, ShellBlock};

pub fn validate(file: &ast::File) -> Result<()> {
    let mut job_names: HashSet<&str> = HashSet::new();
    let mut event_names: HashSet<&str> = HashSet::new();
    let mut once_jobs: HashSet<&str> = HashSet::new();

    // Step 1: Collect names and check duplicates.
    for job in &file.jobs {
        if !job_names.insert(&job.name) {
            bail!("duplicate job name '{}'", job.name);
        }
        if job.body.once == Some(true) {
            once_jobs.insert(&job.name);
        }
    }
    for event in &file.events {
        if !event_names.insert(&event.name) {
            bail!("duplicate event name '{}'", event.name);
        }
    }
    for name in &job_names {
        if event_names.contains(name) {
            bail!("name '{}' is used as both a job and an event", name);
        }
    }

    // Build after-edges for cycle detection and reachability.
    let mut after_edges: HashMap<&str, HashSet<&str>> = HashMap::new();

    // Step 2: Validate after references + build edges.
    for job in &file.jobs {
        let targets = collect_after_targets(&job.body);
        for target in &targets {
            if !job_names.contains(target) {
                bail!(
                    "job '{}': after @{} references unknown job",
                    job.name,
                    target
                );
            }
            if !once_jobs.contains(target) {
                bail!(
                    "job '{}': after @{} target must be once = true",
                    job.name,
                    target
                );
            }
        }
        after_edges.insert(job.name.as_str(), targets);
    }
    for event in &file.events {
        let targets = collect_after_targets(&event.body);
        for target in &targets {
            if !job_names.contains(target) {
                bail!(
                    "event '{}': after @{} references unknown job",
                    event.name,
                    target
                );
            }
            if !once_jobs.contains(target) {
                bail!(
                    "event '{}': after @{} target must be once = true",
                    event.name,
                    target
                );
            }
        }
        after_edges.insert(event.name.as_str(), targets);
    }

    // Step 3: Cycle detection.
    detect_cycles(&after_edges)?;

    // Step 4: Validate @job.KEY output references.
    for job in &file.jobs {
        validate_output_refs(&job.name, &job.body, &once_jobs, &after_edges)?;
    }
    for event in &file.events {
        validate_output_refs(&event.name, &event.body, &once_jobs, &after_edges)?;
    }

    // Step 5: Validate on_fail spawn references.
    for job in &file.jobs {
        validate_spawns(&job.name, &job.body, &job_names, &event_names)?;
    }
    for event in &file.events {
        validate_spawns(&event.name, &event.body, &job_names, &event_names)?;
    }

    // Step 6: Variable shadowing.
    for job in &file.jobs {
        check_variable_shadowing(&job.name, &job.body)?;
    }
    for event in &file.events {
        check_variable_shadowing(&event.name, &event.body)?;
    }

    // Step 7: Duplicate watch names.
    for job in &file.jobs {
        check_duplicate_watches(&job.name, &job.body)?;
    }
    for event in &file.events {
        check_duplicate_watches(&event.name, &event.body)?;
    }

    // Step 8: Empty run rejection.
    for job in &file.jobs {
        check_empty_run(&job.name, &job.body)?;
    }
    for event in &file.events {
        check_empty_run(&event.name, &event.body)?;
    }

    Ok(())
}

fn collect_after_targets(body: &ast::JobBody) -> HashSet<&str> {
    let mut targets = HashSet::new();
    if let Some(wait) = &body.wait {
        for cond in &wait.conditions {
            if let ast::ConditionKind::After { job } = &cond.kind {
                targets.insert(job.as_str());
            }
        }
    }
    targets
}

fn collect_output_refs(expr: &Expr) -> Vec<(&str, &str)> {
    match expr {
        Expr::JobOutputRef(job, key, _) => vec![(job.as_str(), key.as_str())],
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
) -> Result<()> {
    let mut all_refs = Vec::new();
    for env in &body.env {
        all_refs.extend(collect_output_refs(&env.value));
    }
    if let RunSection::ForLoop(fl) = &body.run_section {
        for env in &fl.env {
            all_refs.extend(collect_output_refs(&env.value));
        }
    }
    for (job, _key) in all_refs {
        if !once_jobs.contains(job) {
            bail!(
                "'{}': @{}.* reference requires '{}' to be once = true",
                owner_name,
                job,
                job
            );
        }
        if !is_reachable(owner_name, job, after_edges) {
            bail!(
                "'{}': @{}.* reference requires after @{} (direct or transitive)",
                owner_name,
                job,
                job
            );
        }
    }
    Ok(())
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
    owner_name: &str,
    body: &ast::JobBody,
    job_names: &HashSet<&str>,
    event_names: &HashSet<&str>,
) -> Result<()> {
    for watch in &body.watches {
        if let Some(ast::OnFailAction::Spawn(target)) = &watch.on_fail {
            if !job_names.contains(target.as_str()) && !event_names.contains(target.as_str()) {
                bail!(
                    "'{}': on_fail spawn @{} references unknown target",
                    owner_name,
                    target
                );
            }
            if job_names.contains(target.as_str()) {
                bail!(
                    "'{}': on_fail spawn @{} must reference an event, not a job",
                    owner_name,
                    target
                );
            }
        }
    }
    Ok(())
}

fn check_variable_shadowing(owner_name: &str, body: &ast::JobBody) -> Result<()> {
    let mut vars: HashSet<&str> = HashSet::new();
    if let Some(wait) = &body.wait {
        for cond in &wait.conditions {
            if let ast::ConditionKind::Contains { var: Some(v), .. } = &cond.kind
                && !vars.insert(v.as_str())
            {
                bail!(
                    "'{}': variable '{}' shadows existing variable",
                    owner_name,
                    v
                );
            }
        }
    }
    if let RunSection::ForLoop(fl) = &body.run_section
        && !vars.insert(fl.var.as_str())
    {
        bail!(
            "'{}': for-loop variable '{}' shadows existing variable",
            owner_name,
            fl.var
        );
    }
    Ok(())
}

fn check_duplicate_watches(owner_name: &str, body: &ast::JobBody) -> Result<()> {
    let mut names: HashSet<&str> = HashSet::new();
    for watch in &body.watches {
        if !names.insert(&watch.name) {
            bail!("'{}': duplicate watch name '{}'", owner_name, watch.name);
        }
    }
    Ok(())
}

fn check_empty_run(owner_name: &str, body: &ast::JobBody) -> Result<()> {
    match &body.run_section {
        RunSection::Direct(ShellBlock::Inline(s)) if s.value.is_empty() => {
            bail!("'{}': run command must not be empty", owner_name);
        }
        RunSection::ForLoop(fl) => {
            if let ShellBlock::Inline(s) = &fl.run
                && s.value.is_empty()
            {
                bail!("'{}': run command must not be empty", owner_name);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pman::parser::parse;

    fn parse_and_validate(input: &str) -> Result<()> {
        let file = parse(input)?;
        validate(&file)
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
}
