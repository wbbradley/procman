use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::config::ProcessConfig;

pub struct CommandParser {
    base_env: HashMap<String, String>,
    name_counts: HashMap<String, usize>,
}

impl CommandParser {
    pub fn new() -> Self {
        Self {
            base_env: std::env::vars().collect(),
            name_counts: HashMap::new(),
        }
    }

    /// Parse a single command line string into a ProcessConfig.
    pub fn parse_command_line(&mut self, line: &str) -> Result<ProcessConfig> {
        let tokens = shell_words::split(line).unwrap_or_default();
        if tokens.is_empty() {
            bail!("empty command line");
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

        let mut env = self.base_env.clone();
        for (k, v) in &inline_env {
            env.insert(k.clone(), v.clone());
        }
        for (k, v) in &inline_env {
            let resolved = substitute(v, &env)?;
            env.insert(k.clone(), resolved);
        }

        let program = substitute(&tokens[program_idx], &env)?;
        let args: Vec<String> = tokens[program_idx + 1..]
            .iter()
            .map(|a| substitute(a, &env))
            .collect::<Result<_>>()?;

        let basename = std::path::Path::new(&program)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| program.clone());

        let count = self.name_counts.entry(basename.clone()).or_insert(0);
        let name = if *count == 0 {
            basename.clone()
        } else {
            format!("{basename}.{count}")
        };
        *self.name_counts.get_mut(&basename).unwrap() += 1;

        Ok(ProcessConfig {
            name,
            env,
            program,
            args,
            depends: Vec::new(),
        })
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> CommandParser {
        CommandParser {
            base_env: HashMap::new(),
            name_counts: HashMap::new(),
        }
    }

    #[test]
    fn parse_simple_command() {
        let mut parser = make_parser();
        let cmd = parser.parse_command_line("sleep 5").unwrap();
        assert_eq!(cmd.program, "sleep");
        assert_eq!(cmd.args, vec!["5"]);
        assert_eq!(cmd.name, "sleep");
    }

    #[test]
    fn parse_with_inline_env() {
        let mut parser = make_parser();
        let cmd = parser.parse_command_line("FOO=bar echo hello").unwrap();
        assert_eq!(cmd.program, "echo");
        assert_eq!(cmd.args, vec!["hello"]);
        assert_eq!(cmd.env.get("FOO").unwrap(), "bar");
    }

    #[test]
    fn duplicate_names_get_suffixed() {
        let mut parser = make_parser();
        let cmd1 = parser.parse_command_line("sleep 1").unwrap();
        let cmd2 = parser.parse_command_line("sleep 2").unwrap();
        let cmd3 = parser.parse_command_line("sleep 3").unwrap();
        assert_eq!(cmd1.name, "sleep");
        assert_eq!(cmd2.name, "sleep.1");
        assert_eq!(cmd3.name, "sleep.2");
    }

    #[test]
    fn env_only_line_is_error() {
        let mut parser = make_parser();
        assert!(parser.parse_command_line("FOO=bar").is_err());
    }

    #[test]
    fn empty_line_is_error() {
        let mut parser = make_parser();
        assert!(parser.parse_command_line("").is_err());
    }
}
