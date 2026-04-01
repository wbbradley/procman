use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::config::{ArgDef, ArgType};

fn value_key(def: &ArgDef) -> String {
    match &def.namespace {
        Some(ns) => format!("{ns}::{}", def.name),
        None => def.name.clone(),
    }
}

fn long_flag(def: &ArgDef) -> String {
    match &def.namespace {
        Some(ns) => format!("--{ns}::{}", def.name.replace('_', "-")),
        None => format!("--{}", def.name.replace('_', "-")),
    }
}

pub fn parse_user_args(raw_args: &[String], defs: &[ArgDef]) -> Result<HashMap<String, String>> {
    if defs.is_empty() {
        if !raw_args.is_empty() {
            bail!("unexpected arguments after --: no args defined in config");
        }
        return Ok(HashMap::new());
    }

    // Build lookup from flag string to def index
    let mut long_to_idx: HashMap<String, usize> = HashMap::new();
    let mut short_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, def) in defs.iter().enumerate() {
        long_to_idx.insert(long_flag(def), i);
        if def.namespace.is_none()
            && let Some(ref short) = def.short
        {
            short_to_idx.insert(format!("-{short}"), i);
        }
    }

    let mut values: HashMap<String, String> = HashMap::new();
    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];

        if arg == "--help" {
            print_usage(defs);
            std::process::exit(0);
        }

        let idx = if let Some(&idx) = long_to_idx.get(arg) {
            idx
        } else if let Some(&idx) = short_to_idx.get(arg) {
            idx
        } else {
            bail!("unknown argument: {arg}");
        };

        let def = &defs[idx];
        let key = value_key(def);
        match def.arg_type {
            ArgType::Bool => {
                values.insert(key, "true".to_string());
            }
            ArgType::String => {
                i += 1;
                if i >= raw_args.len() {
                    let flag = long_flag(def);
                    bail!("argument {flag} requires a value");
                }
                values.insert(key, raw_args[i].clone());
            }
        }
        i += 1;
    }

    // Apply defaults and check for missing required args
    for def in defs {
        let key = value_key(def);
        if let std::collections::hash_map::Entry::Vacant(e) = values.entry(key) {
            if let Some(ref default) = def.default {
                e.insert(default.clone());
            } else {
                let flag = long_flag(def);
                bail!("required argument {flag} not provided");
            }
        }
    }

    Ok(values)
}

/// Parse only root-level args (namespace=None) from raw_args.
/// Returns (resolved_root_values, remaining_raw_args).
/// Unmatched tokens are collected into remaining for a second parsing pass.
pub fn parse_root_args(
    raw_args: &[String],
    root_defs: &[ArgDef],
    lenient: bool,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    let mut long_to_idx: HashMap<String, usize> = HashMap::new();
    let mut short_to_idx: HashMap<String, usize> = HashMap::new();
    for (i, def) in root_defs.iter().enumerate() {
        long_to_idx.insert(long_flag(def), i);
        if let Some(ref short) = def.short {
            short_to_idx.insert(format!("-{short}"), i);
        }
    }

    let mut values: HashMap<String, String> = HashMap::new();
    let mut remaining: Vec<String> = Vec::new();
    let mut i = 0;
    while i < raw_args.len() {
        let arg = &raw_args[i];

        let idx = if let Some(&idx) = long_to_idx.get(arg) {
            Some(idx)
        } else if let Some(&idx) = short_to_idx.get(arg) {
            Some(idx)
        } else {
            None
        };

        if let Some(idx) = idx {
            let def = &root_defs[idx];
            let key = value_key(def);
            match def.arg_type {
                ArgType::Bool => {
                    values.insert(key, "true".to_string());
                }
                ArgType::String => {
                    i += 1;
                    if i >= raw_args.len() {
                        let flag = long_flag(def);
                        bail!("argument {flag} requires a value");
                    }
                    values.insert(key, raw_args[i].clone());
                }
            }
        } else {
            remaining.push(arg.clone());
        }
        i += 1;
    }

    // Apply defaults and check for missing required root args.
    for def in root_defs {
        let key = value_key(def);
        if let std::collections::hash_map::Entry::Vacant(e) = values.entry(key) {
            if let Some(ref default) = def.default {
                e.insert(default.clone());
            } else if !lenient {
                let flag = long_flag(def);
                bail!("required argument {flag} not provided");
            }
        }
    }

    Ok((values, remaining))
}

pub fn print_usage(defs: &[ArgDef]) {
    use std::collections::BTreeMap;

    let root_defs: Vec<_> = defs.iter().filter(|d| d.namespace.is_none()).collect();
    let mut ns_groups: BTreeMap<&str, Vec<&ArgDef>> = BTreeMap::new();
    for def in defs.iter().filter(|d| d.namespace.is_some()) {
        ns_groups
            .entry(def.namespace.as_deref().unwrap())
            .or_default()
            .push(def);
    }

    if !root_defs.is_empty() {
        eprintln!("User-defined arguments (passed after --):\n");
        for def in &root_defs {
            print_arg_def(def);
        }
    }
    for (ns, group) in &ns_groups {
        eprintln!("\n  [{ns}]");
        for def in group {
            print_arg_def(def);
        }
    }
}

fn print_arg_def(def: &ArgDef) {
    let long = long_flag(def);
    let short = def
        .short
        .as_ref()
        .map(|s| format!(", -{s}"))
        .unwrap_or_default();
    let type_str = match def.arg_type {
        ArgType::String => " <value>",
        ArgType::Bool => "",
    };
    let desc = def.description.as_deref().unwrap_or("");
    let default = def
        .default
        .as_ref()
        .map(|d| format!(" [default: {d}]"))
        .unwrap_or_default();
    eprintln!("  {long}{short}{type_str}");
    if !desc.is_empty() || !default.is_empty() {
        eprintln!("      {desc}{default}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_def(name: &str, short: Option<&str>, default: Option<&str>) -> ArgDef {
        ArgDef {
            name: name.to_string(),
            namespace: None,
            short: short.map(|s| s.to_string()),
            description: None,
            arg_type: ArgType::String,
            default: default.map(|s| s.to_string()),
            env: None,
        }
    }

    fn bool_def(name: &str, short: Option<&str>, default: Option<&str>) -> ArgDef {
        ArgDef {
            name: name.to_string(),
            namespace: None,
            short: short.map(|s| s.to_string()),
            description: None,
            arg_type: ArgType::Bool,
            default: default.map(|s| s.to_string()),
            env: None,
        }
    }

    fn ns_string_def(name: &str, namespace: &str, default: Option<&str>) -> ArgDef {
        ArgDef {
            name: name.to_string(),
            namespace: Some(namespace.to_string()),
            short: None,
            description: None,
            arg_type: ArgType::String,
            default: default.map(|s| s.to_string()),
            env: None,
        }
    }

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_string_arg_long() {
        let defs = vec![string_def("rust_log", None, None)];
        let result = parse_user_args(&args(&["--rust-log", "debug"]), &defs).unwrap();
        assert_eq!(result.get("rust_log").unwrap(), "debug");
    }

    #[test]
    fn parse_string_arg_short() {
        let defs = vec![string_def("rust_log", Some("r"), None)];
        let result = parse_user_args(&args(&["-r", "debug"]), &defs).unwrap();
        assert_eq!(result.get("rust_log").unwrap(), "debug");
    }

    #[test]
    fn parse_bool_arg_present() {
        let defs = vec![bool_def("enable_feature", None, Some("false"))];
        let result = parse_user_args(&args(&["--enable-feature"]), &defs).unwrap();
        assert_eq!(result.get("enable_feature").unwrap(), "true");
    }

    #[test]
    fn parse_bool_arg_absent_with_default() {
        let defs = vec![bool_def("enable_feature", None, Some("false"))];
        let result = parse_user_args(&args(&[]), &defs).unwrap();
        assert_eq!(result.get("enable_feature").unwrap(), "false");
    }

    #[test]
    fn parse_default_applied() {
        let defs = vec![string_def("log_level", None, Some("info"))];
        let result = parse_user_args(&args(&[]), &defs).unwrap();
        assert_eq!(result.get("log_level").unwrap(), "info");
    }

    #[test]
    fn parse_missing_required_arg_errors() {
        let defs = vec![string_def("required_arg", None, None)];
        let err = parse_user_args(&args(&[]), &defs).unwrap_err();
        assert!(
            format!("{err}").contains("required argument"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_unknown_flag_errors() {
        let defs = vec![string_def("rust_log", None, Some("info"))];
        let err = parse_user_args(&args(&["--nonexistent", "value"]), &defs).unwrap_err();
        assert!(
            format!("{err}").contains("unknown argument"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_missing_value_for_string_errors() {
        let defs = vec![string_def("rust_log", None, None)];
        let err = parse_user_args(&args(&["--rust-log"]), &defs).unwrap_err();
        assert!(
            format!("{err}").contains("requires a value"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_multiple_args() {
        let defs = vec![
            string_def("rust_log", Some("r"), None),
            bool_def("verbose", Some("v"), Some("false")),
            string_def("port", None, Some("8080")),
        ];
        let result = parse_user_args(&args(&["--verbose", "-r", "trace"]), &defs).unwrap();
        assert_eq!(result.get("rust_log").unwrap(), "trace");
        assert_eq!(result.get("verbose").unwrap(), "true");
        assert_eq!(result.get("port").unwrap(), "8080");
    }

    #[test]
    fn parse_empty_args_no_defs() {
        let result = parse_user_args(&args(&[]), &[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn parse_user_args_with_defs_all_defaulted() {
        let defs = vec![
            string_def("a", None, Some("default_a")),
            bool_def("b", None, Some("false")),
        ];
        let result = parse_user_args(&args(&[]), &defs).unwrap();
        assert_eq!(result.get("a").unwrap(), "default_a");
        assert_eq!(result.get("b").unwrap(), "false");
    }

    #[test]
    fn parse_unexpected_args_no_defs_errors() {
        let err = parse_user_args(&args(&["--foo", "bar"]), &[]).unwrap_err();
        assert!(
            format!("{err}").contains("no args defined"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_namespaced_string_arg() {
        let defs = vec![ns_string_def("url", "db", None)];
        let result = parse_user_args(&args(&["--db::url", "postgres://localhost"]), &defs).unwrap();
        assert_eq!(result.get("db::url").unwrap(), "postgres://localhost");
    }

    #[test]
    fn parse_namespaced_required_missing() {
        let defs = vec![ns_string_def("url", "db", None)];
        let err = parse_user_args(&args(&[]), &defs).unwrap_err();
        assert!(
            format!("{err}").contains("required argument --db::url not provided"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_mixed_root_and_namespaced() {
        let defs = vec![
            string_def("log_level", None, Some("info")),
            ns_string_def("url", "db", None),
        ];
        let result = parse_user_args(&args(&["--db::url", "postgres://localhost"]), &defs).unwrap();
        assert_eq!(result.get("log_level").unwrap(), "info");
        assert_eq!(result.get("db::url").unwrap(), "postgres://localhost");
    }

    #[test]
    fn parse_root_args_extracts_known_flags() {
        let root_defs = vec![string_def("log_level", None, Some("info"))];
        let raw = args(&["--log-level", "debug", "--db::url", "pg://host"]);
        let (values, remaining) = parse_root_args(&raw, &root_defs, false).unwrap();
        assert_eq!(values.get("log_level").unwrap(), "debug");
        assert_eq!(remaining, vec!["--db::url", "pg://host"]);
    }

    #[test]
    fn parse_root_args_applies_defaults() {
        let root_defs = vec![string_def("log_level", None, Some("info"))];
        let raw = args(&["--db::url", "pg://host"]);
        let (values, remaining) = parse_root_args(&raw, &root_defs, false).unwrap();
        assert_eq!(values.get("log_level").unwrap(), "info");
        assert_eq!(remaining, vec!["--db::url", "pg://host"]);
    }

    #[test]
    fn parse_root_args_missing_required_errors() {
        let root_defs = vec![string_def("required_arg", None, None)];
        let err = parse_root_args(&args(&[]), &root_defs, false).unwrap_err();
        assert!(format!("{err}").contains("required argument"), "got: {err}");
    }

    #[test]
    fn parse_root_args_no_defs_passes_all_through() {
        let raw = args(&["--db::url", "pg://host"]);
        let (values, remaining) = parse_root_args(&raw, &[], false).unwrap();
        assert!(values.is_empty());
        assert_eq!(remaining, vec!["--db::url", "pg://host"]);
    }
}
