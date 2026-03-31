use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Result, bail};

use crate::pman::{ast, parser};

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
}

pub fn load(root_content: &str, root_path: &str) -> Result<LoadedModules> {
    let root = parser::parse(root_content, root_path)?;

    let mut imports = HashMap::new();
    let mut canonical_to_alias: HashMap<String, String> = HashMap::new();
    let mut seen_aliases: HashSet<String> = HashSet::new();

    let root_dir = Path::new(root_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));

    for import_def in &root.imports {
        let alias = &import_def.alias;

        // Check duplicate aliases.
        if !seen_aliases.insert(alias.clone()) {
            bail!(
                "{}",
                import_def
                    .span
                    .fmt_error(root_path, &format!("duplicate import alias '{alias}'"))
            );
        }

        // Resolve path relative to root file's directory.
        let resolved = root_dir.join(&import_def.path.value);
        let canonical = std::fs::canonicalize(&resolved).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                import_def.span.fmt_error(
                    root_path,
                    &format!("cannot resolve import '{}': {e}", import_def.path.value)
                )
            )
        })?;
        let canonical_str = canonical.to_string_lossy().to_string();

        // Check for diamond imports (same canonical path, different alias).
        if let Some(existing_alias) = canonical_to_alias.get(&canonical_str) {
            bail!(
                "{}",
                import_def.span.fmt_error(
                    root_path,
                    &format!(
                        "import '{}' resolves to the same file as alias '{existing_alias}'",
                        import_def.path.value
                    )
                )
            );
        }
        canonical_to_alias.insert(canonical_str.clone(), alias.clone());

        // Check for cycle (importing self).
        if let Ok(root_canonical) = std::fs::canonicalize(root_path)
            && canonical == root_canonical
        {
            bail!(
                "{}",
                import_def
                    .span
                    .fmt_error(root_path, "import creates a cycle (imports itself)")
            );
        }

        // Read and parse the imported file.
        let imported_content = std::fs::read_to_string(&canonical).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                import_def.span.fmt_error(
                    root_path,
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

        // Validate: no nested imports.
        if let Some(nested_import) = imported_file.imports.first() {
            bail!(
                "{}",
                nested_import.span.fmt_error(
                    &canonical_str,
                    "nested imports are not supported (imported files cannot import other files)"
                )
            );
        }

        imports.insert(
            alias.clone(),
            LoadedModule {
                file: imported_file,
                path: canonical_str,
                alias: alias.clone(),
                bindings: import_def.bindings.clone(),
            },
        );
    }

    Ok(LoadedModules {
        root,
        root_path: root_path.to_string(),
        imports,
    })
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
    fn load_nested_import_rejected() {
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
        let err = load(&content, root_path.to_str().unwrap()).unwrap_err();
        assert!(
            err.to_string().contains("nested imports are not supported"),
            "got: {err}"
        );
    }
}
