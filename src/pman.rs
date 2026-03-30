mod ast;
mod expr;
mod lexer;
pub mod loader;
mod lower;
mod parser;
mod token;
mod validate;

use std::collections::HashMap;

use anyhow::Result;

use crate::config;

pub fn parse(
    input: &str,
    path: &str,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<crate::config::ProcessConfig>, Option<String>)> {
    let modules = loader::load(input, path)?;
    lower::lower_modules(&modules, extra_env, arg_values)
}

pub fn parse_header(input: &str, path: &str) -> Result<config::ConfigHeader> {
    let file = parser::parse(input, path)?;
    let log_dir = file
        .config
        .as_ref()
        .and_then(|c| c.logs.as_ref().map(|l| l.value.clone()));
    let log_time = file
        .config
        .as_ref()
        .and_then(|c| c.log_time)
        .unwrap_or(false);
    let arg_defs = file
        .args
        .into_iter()
        .map(lower::lower_arg_def)
        .collect::<Result<_>>()?;
    Ok(config::ConfigHeader {
        log_dir,
        log_time,
        arg_defs,
    })
}
