mod ast;
mod expr;
mod lexer;
mod lower;
mod parser;
mod token;
mod validate;

use std::collections::HashMap;

use anyhow::Result;

use crate::config_parser;

pub fn parse(
    input: &str,
    path: &str,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<crate::config::ProcessConfig>, Option<String>)> {
    lower::lower(input, path, extra_env, arg_values)
}

pub fn parse_header(input: &str, path: &str) -> Result<config_parser::ConfigHeader> {
    let file = parser::parse(input, path)?;
    let log_dir = file
        .config
        .as_ref()
        .and_then(|c| c.logs.as_ref().map(|l| l.value.clone()));
    let arg_defs = match file.config {
        Some(config) => config
            .args
            .into_iter()
            .map(lower::lower_arg_def)
            .collect::<Result<_>>()?,
        None => Vec::new(),
    };
    Ok(config_parser::ConfigHeader { log_dir, arg_defs })
}
