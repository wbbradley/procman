use std::collections::HashMap;

use anyhow::{Context, Result, bail};

pub struct Procfile {
    pub commands: Vec<Command>,
}

pub struct Command {
    pub env: HashMap<String, String>,
    pub program: String,
    pub args: Vec<String>,
    pub name: String,
}

fn is_env_assignment(token: &str) -> Option<(&str, &str)> {
    let eq = token.find('=')?;
    let key = &token[..eq];
    if key.is_empty() {
        return None;
    }
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some((key, &token[eq + 1..]))
}

fn substitute(s: &str, env: &HashMap<String, String>) -> Result<String> {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut var_name = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_alphanumeric() || nc == '_' {
                    var_name.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            if var_name.is_empty() {
                result.push('$');
            } else {
                let val = env
                    .get(&var_name)
                    .with_context(|| format!("undefined variable: ${var_name}"))?;
                result.push_str(val);
            }
        } else {
            result.push(c);
        }
    }
    Ok(result)
}

pub fn parse(path: &str) -> Result<Procfile> {
    let content = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;

    // Join continuation lines
    let content = content.replace("\\\n", "");

    // Collect non-blank, non-comment lines
    let lines: Vec<&str> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();

    // Phase 1: split into global env lines and command lines
    let mut global_env: HashMap<String, String> = HashMap::new();
    let mut command_lines: Vec<&str> = Vec::new();
    let mut seen_command = false;

    for line in &lines {
        if !seen_command {
            // Check if entire line is a simple KEY=value (no spaces before '=')
            if let Some((key, val)) = is_env_assignment(line) {
                global_env.insert(key.to_string(), val.to_string());
                continue;
            }
        }
        seen_command = true;
        command_lines.push(line);
    }

    // Build base env: inherit from process env, then overlay global vars.
    // Only substitute in global env values (not inherited ones, which may contain $-literals).
    let mut base_env: HashMap<String, String> = std::env::vars().collect();
    for (k, v) in &global_env {
        base_env.insert(k.clone(), substitute(v, &base_env)?);
    }

    // Phase 2: parse command lines
    let mut commands = Vec::new();
    let mut name_counts: HashMap<String, usize> = HashMap::new();

    for line in &command_lines {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }

        let mut inline_env: HashMap<String, String> = HashMap::new();
        let mut program_idx = 0;

        for (i, token) in tokens.iter().enumerate() {
            if let Some((key, val)) = is_env_assignment(token) {
                inline_env.insert(key.to_string(), val.to_string());
                program_idx = i + 1;
            } else {
                break;
            }
        }

        if program_idx >= tokens.len() {
            bail!("no program found in line: {line}");
        }

        // Merge env: base → inline
        let mut env = base_env.clone();
        for (k, v) in &inline_env {
            env.insert(k.clone(), v.clone());
        }

        // Substitute in inline env values
        for (k, v) in &inline_env {
            let resolved = substitute(v, &env)?;
            env.insert(k.clone(), resolved);
        }

        // Substitute program and args
        let program = substitute(tokens[program_idx], &env)?;
        let args: Vec<String> = tokens[program_idx + 1..]
            .iter()
            .map(|a| substitute(a, &env))
            .collect::<Result<_>>()?;

        // Derive name from program basename
        let basename = std::path::Path::new(&program)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| program.clone());

        let count = name_counts.entry(basename.clone()).or_insert(0);
        let name = if *count == 0 {
            basename.clone()
        } else {
            format!("{basename}.{count}")
        };
        *name_counts.get_mut(&basename).unwrap() += 1;

        commands.push(Command {
            env,
            program,
            args,
            name,
        });
    }

    if commands.is_empty() {
        bail!("no commands found in {path}");
    }

    Ok(Procfile { commands })
}
