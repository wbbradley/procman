use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::config::{ArgDef, ArgType};

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
        let long_flag = def.name.replace('_', "-");
        long_to_idx.insert(format!("--{long_flag}"), i);
        if let Some(ref short) = def.short {
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
        match def.arg_type {
            ArgType::Bool => {
                values.insert(def.name.clone(), "true".to_string());
            }
            ArgType::String => {
                i += 1;
                if i >= raw_args.len() {
                    bail!("argument --{} requires a value", def.name.replace('_', "-"));
                }
                values.insert(def.name.clone(), raw_args[i].clone());
            }
        }
        i += 1;
    }

    // Apply defaults and check for missing required args
    for def in defs {
        if !values.contains_key(&def.name) {
            if let Some(ref default) = def.default {
                values.insert(def.name.clone(), default.clone());
            } else {
                bail!(
                    "required argument --{} not provided",
                    def.name.replace('_', "-")
                );
            }
        }
    }

    Ok(values)
}

fn print_usage(defs: &[ArgDef]) {
    eprintln!("User-defined arguments (passed after --):\n");
    for def in defs {
        let long = format!("--{}", def.name.replace('_', "-"));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_def(name: &str, short: Option<&str>, default: Option<&str>) -> ArgDef {
        ArgDef {
            name: name.to_string(),
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
            short: short.map(|s| s.to_string()),
            description: None,
            arg_type: ArgType::Bool,
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
}
