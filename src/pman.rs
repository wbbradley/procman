mod ast;
mod expr;
mod lexer;
pub mod loader;
mod lower;
mod parser;
mod token;
mod validate;

use std::collections::{HashMap, HashSet};

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

/// Load all modules and collect arg definitions (including unbound imported args).
pub fn load_header(
    input: &str,
    path: &str,
) -> Result<(loader::LoadedModules, config::ConfigHeader)> {
    let modules = loader::load(input, path)?;

    let log_dir = modules
        .root
        .config
        .as_ref()
        .and_then(|c| c.logs.as_ref().map(|l| l.value.clone()));
    let log_time = modules
        .root
        .config
        .as_ref()
        .and_then(|c| c.log_time)
        .unwrap_or(false);

    // Root arg defs (namespace=None).
    let mut arg_defs: Vec<config::ArgDef> = modules
        .root
        .args
        .iter()
        .map(|a| lower::lower_arg_def_ref(a, None))
        .collect::<Result<_>>()?;

    // Imported module unbound arg defs (namespace=Some(alias)).
    for import_def in &modules.root.imports {
        let alias = &import_def.alias;
        if let Some(module) = modules.imports.get(alias) {
            let bound_names: HashSet<&str> = import_def
                .bindings
                .iter()
                .map(|b| b.name.as_str())
                .collect();
            for arg_def in &module.file.args {
                let has_binding = bound_names.contains(arg_def.name.as_str());
                let has_default = arg_def.default.is_some();
                if !has_binding && !has_default {
                    arg_defs.push(lower::lower_arg_def_ref(arg_def, Some(alias))?);
                }
            }
        }
    }

    Ok((
        modules,
        config::ConfigHeader {
            log_dir,
            log_time,
            arg_defs,
        },
    ))
}

/// Lower pre-loaded modules with resolved arg values.
pub fn lower_loaded(
    modules: &loader::LoadedModules,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<crate::config::ProcessConfig>, Option<String>)> {
    lower::lower_modules(modules, extra_env, arg_values)
}
