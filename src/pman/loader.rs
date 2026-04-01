use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Result, bail};

use crate::pman::{ast, parser, token::Span};

#[derive(Debug)]
pub struct LoadedModules {
    pub root: ast::File,
    pub root_path: String,
    pub imports: HashMap<String, LoadedModule>,
}

#[derive(Debug)]
pub struct LoadedModule {
    pub file: ast::File,
    pub path: String,
    #[allow(dead_code)]
    pub alias: String,
    pub bindings: Vec<ast::ImportBinding>,
    pub imports: HashMap<String, LoadedModule>,
}

pub fn load(root_content: &str, root_path: &str) -> Result<LoadedModules> {
    let root = parser::parse(root_content, root_path)?;
    load_with_root(root, root_path, &HashMap::new())
}

/// Load imports given an already-parsed root file and root arg values for path substitution.
pub fn load_with_root(
    root: ast::File,
    root_path: &str,
    root_arg_values: &HashMap<String, String>,
) -> Result<LoadedModules> {
    let root_canonical =
        std::fs::canonicalize(root_path).unwrap_or_else(|_| std::path::PathBuf::from(root_path));
    let mut visited = HashSet::new();
    visited.insert(root_canonical.to_string_lossy().to_string());

    let imports = load_imports(&root.imports, root_path, &mut visited, root_arg_values)?;

    Ok(LoadedModules {
        root,
        root_path: root_path.to_string(),
        imports,
    })
}

/// Substitute `${args.NAME}` references in an import path string.
fn substitute_args_in_path(
    raw_path: &str,
    arg_values: &HashMap<String, String>,
    span: Span,
    file_path: &str,
) -> Result<String> {
    let mut result = String::with_capacity(raw_path.len());
    let mut rest = raw_path;
    let prefix = "${args.";
    while let Some(start) = rest.find(prefix) {
        result.push_str(&rest[..start]);
        let after_prefix = &rest[start + prefix.len()..];
        let end = after_prefix.find('}').ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                span.fmt_error(file_path, "unterminated ${args.} reference in import path")
            )
        })?;
        let name = &after_prefix[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!(
                "{}",
                span.fmt_error(
                    file_path,
                    &format!("invalid arg name '{name}' in import path")
                )
            );
        }
        let value = arg_values.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                span.fmt_error(
                    file_path,
                    &format!(
                        "unknown arg '{name}' in import path; only root-level args can be used"
                    )
                )
            )
        })?;
        result.push_str(value);
        rest = &after_prefix[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

fn load_imports(
    import_defs: &[ast::ImportDef],
    parent_path: &str,
    visited: &mut HashSet<String>,
    root_arg_values: &HashMap<String, String>,
) -> Result<HashMap<String, LoadedModule>> {
    let mut imports = HashMap::new();
    let mut canonical_to_alias: HashMap<String, String> = HashMap::new();
    let mut seen_aliases: HashSet<String> = HashSet::new();

    let parent_dir = Path::new(parent_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));

    for import_def in import_defs {
        let alias = &import_def.alias;

        // Check duplicate aliases.
        if !seen_aliases.insert(alias.clone()) {
            bail!(
                "{}",
                import_def
                    .span
                    .fmt_error(parent_path, &format!("duplicate import alias '{alias}'"))
            );
        }

        // Substitute ${args.NAME} references in import path.
        let substituted_path = substitute_args_in_path(
            &import_def.path.value,
            root_arg_values,
            import_def.span,
            parent_path,
        )?;

        // Resolve path relative to parent file's directory.
        let resolved = parent_dir.join(&substituted_path);
        let canonical = std::fs::canonicalize(&resolved).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                import_def.span.fmt_error(
                    parent_path,
                    &format!("cannot resolve import '{}': {e}", import_def.path.value)
                )
            )
        })?;
        let canonical_str = canonical.to_string_lossy().to_string();

        // Check for diamond imports (same canonical path, different alias within this module).
        if let Some(existing_alias) = canonical_to_alias.get(&canonical_str) {
            bail!(
                "{}",
                import_def.span.fmt_error(
                    parent_path,
                    &format!(
                        "import '{}' resolves to the same file as alias '{existing_alias}'",
                        import_def.path.value
                    )
                )
            );
        }
        canonical_to_alias.insert(canonical_str.clone(), alias.clone());

        // Check for cycle (file already on the current import stack).
        if visited.contains(&canonical_str) {
            bail!(
                "{}",
                import_def
                    .span
                    .fmt_error(parent_path, "import creates a cycle")
            );
        }

        // Read and parse the imported file.
        let imported_content = std::fs::read_to_string(&canonical).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                import_def.span.fmt_error(
                    parent_path,
                    &format!("cannot read import '{}': {e}", import_def.path.value)
                )
            )
        })?;
        let imported_file = parser::parse(&imported_content, &canonical_str)?;

        // Validate: no config block in imported file.
        if let Some(config) = &imported_file.config {
            bail!(
                "{}",
                config.span.fmt_error(
                    &canonical_str,
                    "config block is not allowed in imported files"
                )
            );
        }

        // Recursively load nested imports (with cycle detection via visited set).
        visited.insert(canonical_str.clone());
        let sub_imports = load_imports(
            &imported_file.imports,
            &canonical_str,
            visited,
            &HashMap::new(),
        )?;
        visited.remove(&canonical_str);

        imports.insert(
            alias.clone(),
            LoadedModule {
                file: imported_file,
                path: canonical_str,
                alias: alias.clone(),
                bindings: import_def.bindings.clone(),
                imports: sub_imports,
            },
        );
    }

    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_no_imports() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(&root_path, r#"job web { run "serve" }"#).unwrap();
        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert!(modules.imports.is_empty());
        assert_eq!(modules.root.jobs.len(), 1);
    }

    #[test]
    fn load_single_import() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("db.pman");
        std::fs::write(&lib_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert_eq!(modules.imports.len(), 1);
        assert!(modules.imports.contains_key("db"));
        assert_eq!(modules.imports["db"].file.jobs.len(), 1);
    }

    #[test]
    fn load_cycle_detected() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        // Root imports itself.
        std::fs::write(
            &root_path,
            r#"
            import "root.pman" as root
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("cycle"), "got: {err}");
    }

    #[test]
    fn load_config_block_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("lib.pman");
        std::fs::write(&lib_path, r#"config { logs = "./bad" } job x { run "x" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("config block is not allowed"),
            "got: {err}"
        );
    }

    #[test]
    fn load_duplicate_alias() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("db.pman");
        std::fs::write(&lib_path, r#"job migrate { run "migrate" }"#).unwrap();
        let lib2_path = dir.path().join("db2.pman");
        std::fs::write(&lib2_path, r#"job other { run "other" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            import "db2.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("duplicate import alias"),
            "got: {err}"
        );
    }

    #[test]
    fn load_relative_path_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let lib_path = sub.join("lib.pman");
        std::fs::write(&lib_path, r#"job setup { run "setup" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "sub/lib.pman" as lib
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert!(modules.imports.contains_key("lib"));
    }

    #[test]
    fn load_alias_derivation() {
        let dir = tempfile::tempdir().unwrap();
        let lib_path = dir.path().join("database.pman");
        std::fs::write(&lib_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "database.pman"
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert!(
            modules.imports.contains_key("database"),
            "expected alias 'database', got keys: {:?}",
            modules.imports.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn load_import_with_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(
            &db_path,
            r#"
            arg url { type = string }
            job migrate { run "migrate" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db { url = "postgres://localhost/mydb" }
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert_eq!(modules.imports["db"].bindings.len(), 1);
        assert_eq!(modules.imports["db"].bindings[0].name, "url");
    }

    #[test]
    fn load_diamond_import_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "db.pman" as db
            import "db.pman" as db2
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("same file"), "got: {err}");
    }

    #[test]
    fn load_diamond_import_via_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let db_path = sub.join("db.pman");
        std::fs::write(&db_path, r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "sub/db.pman" as db
            import "./sub/db.pman" as db2
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("same file"), "got: {err}");
    }

    #[test]
    fn load_nested_import_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let inner_path = dir.path().join("inner.pman");
        std::fs::write(&inner_path, r#"job inner { run "inner" }"#).unwrap();

        let lib_path = dir.path().join("lib.pman");
        std::fs::write(
            &lib_path,
            r#"
            import "inner.pman" as inner
            job setup { run "setup" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert_eq!(modules.imports.len(), 1);
        assert!(modules.imports.contains_key("lib"));
        let lib = &modules.imports["lib"];
        assert_eq!(lib.imports.len(), 1);
        assert!(lib.imports.contains_key("inner"));
        assert_eq!(lib.imports["inner"].file.jobs.len(), 1);
    }

    #[test]
    fn load_transitive_cycle_detected() {
        let dir = tempfile::tempdir().unwrap();

        // A -> B -> C -> A
        let a_path = dir.path().join("a.pman");
        let b_path = dir.path().join("b.pman");
        let c_path = dir.path().join("c.pman");

        std::fs::write(
            &a_path,
            r#"
            import "b.pman" as b
            job a { run "a" }
            "#,
        )
        .unwrap();
        std::fs::write(
            &b_path,
            r#"
            import "c.pman" as c
            job b { run "b" }
            "#,
        )
        .unwrap();
        std::fs::write(
            &c_path,
            r#"
            import "a.pman" as a
            job c { run "c" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&a_path).unwrap();
        let err = load(&content, a_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("cycle"), "got: {err}");
    }

    #[test]
    fn load_nested_diamond_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.pman");
        std::fs::write(&shared_path, r#"job shared { run "shared" }"#).unwrap();

        // lib.pman imports shared twice under different aliases.
        let lib_path = dir.path().join("lib.pman");
        std::fs::write(
            &lib_path,
            r#"
            import "shared.pman" as s1
            import "shared.pman" as s2
            job lib { run "lib" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "lib.pman" as lib
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("same file"), "got: {err}");
    }

    #[test]
    fn load_same_file_different_parents_ok() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.pman");
        std::fs::write(&shared_path, r#"job shared { run "shared" }"#).unwrap();

        // Both a.pman and b.pman import shared.pman independently.
        let a_path = dir.path().join("a.pman");
        std::fs::write(
            &a_path,
            r#"
            import "shared.pman" as shared
            job a { run "a" }
            "#,
        )
        .unwrap();
        let b_path = dir.path().join("b.pman");
        std::fs::write(
            &b_path,
            r#"
            import "shared.pman" as shared
            job b { run "b" }
            "#,
        )
        .unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "a.pman" as a
            import "b.pman" as b
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let modules = load(&content, root_path.to_str().unwrap()).unwrap();
        assert!(modules.imports["a"].imports.contains_key("shared"));
        assert!(modules.imports["b"].imports.contains_key("shared"));
    }

    #[test]
    fn load_import_with_args_in_path() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("libs");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("db.pman"), r#"job migrate { run "migrate" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "${args.lib_dir}/db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let root = parser::parse(&content, root_path.to_str().unwrap()).unwrap();
        let mut arg_values = HashMap::new();
        arg_values.insert("lib_dir".to_string(), "libs".to_string());
        let modules = load_with_root(root, root_path.to_str().unwrap(), &arg_values).unwrap();
        assert!(modules.imports.contains_key("db"));
    }

    #[test]
    fn load_import_unknown_arg_in_path_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "${args.nonexistent}/db.pman" as db
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let root = parser::parse(&content, root_path.to_str().unwrap()).unwrap();
        let err = load_with_root(root, root_path.to_str().unwrap(), &HashMap::new()).unwrap_err();
        assert!(
            err.to_string().contains("unknown arg 'nonexistent'"),
            "got: {err}"
        );
    }

    #[test]
    fn load_import_auto_alias_with_args_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("mydir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("database.pman"), r#"job x { run "x" }"#).unwrap();

        let root_path = dir.path().join("root.pman");
        std::fs::write(
            &root_path,
            r#"
            import "${args.dir}/database.pman"
            job web { run "serve" }
            "#,
        )
        .unwrap();

        let content = std::fs::read_to_string(&root_path).unwrap();
        let root = parser::parse(&content, root_path.to_str().unwrap()).unwrap();
        let mut args = HashMap::new();
        args.insert("dir".to_string(), "mydir".to_string());
        let modules = load_with_root(root, root_path.to_str().unwrap(), &args).unwrap();
        assert!(
            modules.imports.contains_key("database"),
            "got keys: {:?}",
            modules.imports.keys().collect::<Vec<_>>()
        );
    }
}
