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
pub use lower::ModuleArgsReport;

use crate::config;

#[cfg(test)]
pub fn parse(
    input: &str,
    path: &str,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(Vec<crate::config::ProcessConfig>, Option<String>)> {
    let modules = loader::load(input, path)?;
    let (configs, log_dir, _) = lower::lower_modules(&modules, extra_env, arg_values)?;
    Ok((configs, log_dir))
}

/// Parse a .pman file without loading imports.
pub fn parse_root(input: &str, path: &str) -> Result<ast::File> {
    parser::parse(input, path)
}

/// Collect root-level arg definitions (namespace=None) from a parsed file.
pub fn collect_root_arg_defs(root: &ast::File, root_path: &str) -> Result<Vec<config::ArgDef>> {
    let dir = lower::parent_dir_of(root_path);
    let mut dir_context = HashMap::new();
    dir_context.insert("__procman_dir__".to_string(), dir.clone());
    dir_context.insert("__module_dir__".to_string(), dir);

    // Topo-sort and evaluate defaults incrementally so inter-arg refs work.
    let sorted = lower::topo_sort_args(&root.args)?;
    let mut defs = vec![None; root.args.len()];
    for idx in &sorted {
        let arg = &root.args[*idx];
        let def = lower::lower_arg_def_ref(arg, None, &dir_context)?;
        if let Some(ref val) = def.default {
            dir_context.insert(arg.name.clone(), val.clone());
        }
        defs[*idx] = Some(def);
    }
    Ok(defs.into_iter().map(|d| d.unwrap()).collect())
}

/// Load imports with arg substitution in paths, then collect all arg defs.
pub fn load_with_args(
    root: ast::File,
    path: &str,
    root_arg_values: &HashMap<String, String>,
    check_mode: bool,
) -> Result<(loader::LoadedModules, config::ConfigHeader)> {
    let modules = loader::load_with_root(root, path, root_arg_values, check_mode)?;
    let header = build_config_header(&modules)?;
    Ok((modules, header))
}

/// Load all modules and collect arg definitions (including unbound imported args).
/// Uses literal import paths only (no arg substitution).
pub fn load_header(
    input: &str,
    path: &str,
) -> Result<(loader::LoadedModules, config::ConfigHeader)> {
    let modules = loader::load(input, path)?;
    let header = build_config_header(&modules)?;
    Ok((modules, header))
}

fn build_config_header(modules: &loader::LoadedModules) -> Result<config::ConfigHeader> {
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

    let root_dir = lower::parent_dir_of(&modules.root_path);
    let mut root_dir_context = HashMap::new();
    root_dir_context.insert("__procman_dir__".to_string(), root_dir.clone());
    root_dir_context.insert("__module_dir__".to_string(), root_dir);

    // Root arg defs (namespace=None).
    let mut arg_defs: Vec<config::ArgDef> = modules
        .root
        .args
        .iter()
        .map(|a| lower::lower_arg_def_ref(a, None, &root_dir_context))
        .collect::<Result<_>>()?;

    // Imported module unbound arg defs (namespace=Some(alias)).
    for import_def in &modules.root.imports {
        let alias = &import_def.alias;
        if let Some(module) = modules.imports.get(alias) {
            let module_dir = lower::parent_dir_of(&module.path);
            let mut mod_dir_context = HashMap::new();
            mod_dir_context.insert(
                "__procman_dir__".to_string(),
                root_dir_context["__procman_dir__"].clone(),
            );
            mod_dir_context.insert("__module_dir__".to_string(), module_dir);

            let bound_names: HashSet<&str> = import_def
                .bindings
                .iter()
                .map(|b| b.name.as_str())
                .collect();
            for arg_def in &module.file.args {
                let has_binding = bound_names.contains(arg_def.name.as_str());
                let has_default = arg_def.default.is_some();
                if !has_binding && !has_default {
                    arg_defs.push(lower::lower_arg_def_ref(
                        arg_def,
                        Some(alias),
                        &mod_dir_context,
                    )?);
                }
            }
        }
    }

    Ok(config::ConfigHeader {
        log_dir,
        log_time,
        arg_defs,
    })
}

/// Lower pre-loaded modules with resolved arg values.
pub fn lower_loaded(
    modules: &loader::LoadedModules,
    extra_env: &HashMap<String, String>,
    arg_values: &HashMap<String, String>,
) -> Result<(
    Vec<crate::config::ProcessConfig>,
    Option<String>,
    ModuleArgsReport,
)> {
    lower::lower_modules(modules, extra_env, arg_values)
}
