use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, bail};

use crate::config::{Dependency, ProcessConfig};

pub fn parse_output_file(path: &Path) -> Result<HashMap<String, String>> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading output file {path:?}"))?;
    let mut map = HashMap::new();
    let mut lines = content.lines().peekable();
    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }
        if let Some((key, rest)) = line.split_once("<<") {
            let key = key.trim();
            let delim = rest.trim();
            let mut value_lines = Vec::new();
            for inner in lines.by_ref() {
                if inner.trim() == delim {
                    break;
                }
                value_lines.push(inner);
            }
            map.insert(key.to_string(), value_lines.join("\n"));
        } else if let Some((key, value)) = line.split_once('=') {
            map.insert(key.trim().to_string(), value.to_string());
        }
    }
    Ok(map)
}

pub fn extract_template_refs(s: &str) -> Vec<(String, String)> {
    let mut refs = Vec::new();
    let mut remaining = s;
    while let Some(start) = remaining.find("${{") {
        let after_open = &remaining[start + 3..];
        if let Some(end) = after_open.find("}}") {
            let inner = after_open[..end].trim();
            if let Some((proc_name, key)) = inner.split_once('.') {
                refs.push((proc_name.trim().to_string(), key.trim().to_string()));
            }
            remaining = &after_open[end + 2..];
        } else {
            break;
        }
    }
    refs
}

pub fn resolve_templates(
    s: &str,
    resolver: &impl Fn(&str, &str) -> Result<String>,
) -> Result<String> {
    let mut result = String::new();
    let mut remaining = s;
    while let Some(start) = remaining.find("${{") {
        result.push_str(&remaining[..start]);
        let after_open = &remaining[start + 3..];
        if let Some(end) = after_open.find("}}") {
            let inner = after_open[..end].trim();
            if let Some((proc_name, key)) = inner.split_once('.') {
                let value = resolver(proc_name.trim(), key.trim())?;
                result.push_str(&value);
            } else {
                bail!("invalid template reference: '{inner}' (expected 'process.key')");
            }
            remaining = &after_open[end + 2..];
        } else {
            result.push_str(&remaining[..start + 3]);
            remaining = after_open;
        }
    }
    result.push_str(remaining);
    Ok(result)
}

pub fn validate_config_templates(configs: &[ProcessConfig]) -> Result<()> {
    let config_map: HashMap<&str, &ProcessConfig> =
        configs.iter().map(|c| (c.name.as_str(), c)).collect();

    for config in configs {
        let mut all_refs = Vec::new();
        for value in config.env.values() {
            all_refs.extend(extract_template_refs(value));
        }
        all_refs.extend(extract_template_refs(&config.run));

        for (proc_name, key) in &all_refs {
            // Rule 1: referenced process must exist
            let referenced = config_map.get(proc_name.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "process '{}' references output '${{{{ {proc_name}.{key} }}}}' but process '{proc_name}' does not exist",
                    config.name
                )
            })?;

            // Rule 2: referenced process must be once: true
            if !referenced.once {
                bail!(
                    "process '{}' references output '${{{{ {proc_name}.{key} }}}}' but process '{proc_name}' is not once: true",
                    config.name
                );
            }

            // Rule 3: referencing process must have a (transitive) process_exited dep on the referenced process
            if !has_transitive_process_exited_dep(&config.depends, proc_name, &config_map) {
                bail!(
                    "process '{}' references output '${{{{ {proc_name}.{key} }}}}' but does not have a process_exited dependency (direct or transitive) on '{proc_name}'",
                    config.name
                );
            }
        }
    }
    Ok(())
}

fn has_transitive_process_exited_dep(
    depends: &[Dependency],
    target: &str,
    config_map: &HashMap<&str, &ProcessConfig>,
) -> bool {
    for dep in depends {
        if let Dependency::ProcessExited { name } = dep {
            if name == target {
                return true;
            }
            // Walk transitively
            if let Some(intermediate) = config_map.get(name.as_str())
                && has_transitive_process_exited_dep(&intermediate.depends, target, config_map)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {

    use super::*;

    fn write_temp_file(content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("procman_output_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("test_{}.output", rand_id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn rand_id() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    fn make_config_for_test(
        name: &str,
        run: &str,
        once: bool,
        depends: Vec<Dependency>,
        env: HashMap<String, String>,
    ) -> ProcessConfig {
        ProcessConfig {
            name: name.to_string(),
            env,
            run: run.to_string(),
            depends,
            once,
            for_each: None,
        }
    }

    // --- parse_output_file tests ---

    #[test]
    fn parse_simple_key_value() {
        let path = write_temp_file("a=1\nb=2\n");
        let map = parse_output_file(&path).unwrap();
        assert_eq!(map.get("a").unwrap(), "1");
        assert_eq!(map.get("b").unwrap(), "2");
        assert_eq!(map.len(), 2);
    }

    #[test]
    fn parse_heredoc() {
        let path = write_temp_file("data<<EOF\n{\"json\": true}\nEOF\n");
        let map = parse_output_file(&path).unwrap();
        assert_eq!(map.get("data").unwrap(), "{\"json\": true}");
    }

    #[test]
    fn parse_mixed() {
        let path = write_temp_file("simple=value\nblock<<END\nline1\nline2\nEND\n");
        let map = parse_output_file(&path).unwrap();
        assert_eq!(map.get("simple").unwrap(), "value");
        assert_eq!(map.get("block").unwrap(), "line1\nline2");
    }

    #[test]
    fn parse_empty_file() {
        let path = write_temp_file("");
        let map = parse_output_file(&path).unwrap();
        assert!(map.is_empty());
    }

    // --- extract_template_refs tests ---

    #[test]
    fn extract_refs_finds_all() {
        let refs = extract_template_refs("${{ a.b }} and ${{ c.d }}");
        assert_eq!(
            refs,
            vec![
                ("a".to_string(), "b".to_string()),
                ("c".to_string(), "d".to_string())
            ]
        );
    }

    #[test]
    fn extract_refs_empty() {
        let refs = extract_template_refs("no templates here");
        assert!(refs.is_empty());
    }

    // --- resolve_templates tests ---

    #[test]
    fn resolve_substitutes_values() {
        let result = resolve_templates("hello ${{ p.k }}", &|name, key| {
            assert_eq!(name, "p");
            assert_eq!(key, "k");
            Ok("world".to_string())
        })
        .unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn resolve_no_templates() {
        let result =
            resolve_templates("no templates", &|_, _| panic!("should not be called")).unwrap();
        assert_eq!(result, "no templates");
    }

    #[test]
    fn resolve_missing_key_errors() {
        let result =
            resolve_templates("${{ p.missing }}", &|_, key| bail!("key '{key}' not found"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    // --- validate_config_templates tests ---

    #[test]
    fn validate_valid_config() {
        let configs = vec![
            make_config_for_test("setup", "echo done", true, vec![], HashMap::new()),
            make_config_for_test(
                "app",
                "echo ${{ setup.DB_URL }}",
                false,
                vec![Dependency::ProcessExited {
                    name: "setup".to_string(),
                }],
                HashMap::new(),
            ),
        ];
        validate_config_templates(&configs).unwrap();
    }

    #[test]
    fn validate_unknown_process() {
        let configs = vec![make_config_for_test(
            "app",
            "echo ${{ nonexistent.key }}",
            false,
            vec![],
            HashMap::new(),
        )];
        let err = validate_config_templates(&configs).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn validate_non_once_process() {
        let configs = vec![
            make_config_for_test("server", "run-server", false, vec![], HashMap::new()),
            make_config_for_test(
                "app",
                "echo ${{ server.PORT }}",
                false,
                vec![Dependency::ProcessExited {
                    name: "server".to_string(),
                }],
                HashMap::new(),
            ),
        ];
        let err = validate_config_templates(&configs).unwrap_err();
        assert!(err.to_string().contains("not once: true"), "{err}");
    }

    #[test]
    fn validate_missing_dep_chain() {
        let configs = vec![
            make_config_for_test("setup", "echo done", true, vec![], HashMap::new()),
            make_config_for_test(
                "app",
                "echo ${{ setup.DB_URL }}",
                false,
                vec![], // no dependency on setup
                HashMap::new(),
            ),
        ];
        let err = validate_config_templates(&configs).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not have a process_exited dependency"),
            "{err}"
        );
    }

    #[test]
    fn validate_transitive_dep() {
        let configs = vec![
            make_config_for_test("setup", "echo done", true, vec![], HashMap::new()),
            make_config_for_test(
                "middle",
                "echo middle",
                true,
                vec![Dependency::ProcessExited {
                    name: "setup".to_string(),
                }],
                HashMap::new(),
            ),
            make_config_for_test(
                "app",
                "echo ${{ setup.DB_URL }}",
                false,
                vec![Dependency::ProcessExited {
                    name: "middle".to_string(),
                }],
                HashMap::new(),
            ),
        ];
        validate_config_templates(&configs).unwrap();
    }

    #[test]
    fn validate_template_in_env() {
        let mut env = HashMap::new();
        env.insert("DB".to_string(), "${{ setup.DB_URL }}".to_string());
        let configs = vec![
            make_config_for_test("setup", "echo done", true, vec![], HashMap::new()),
            make_config_for_test(
                "app",
                "echo app",
                false,
                vec![Dependency::ProcessExited {
                    name: "setup".to_string(),
                }],
                env,
            ),
        ];
        validate_config_templates(&configs).unwrap();
    }
}
