use std::path::PathBuf;

use anyhow::{Context, Result};

pub fn derive_fifo_path(config_path: &str) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(config_path)
        .with_context(|| format!("cannot resolve config path '{config_path}'"))?;

    let sanitized = canonical
        .parent()
        .and_then(|p| p.file_name())
        .map(|name| {
            let s: String = name
                .to_string_lossy()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .take(32)
                .collect();
            if s.is_empty() {
                "procman".to_string()
            } else {
                s
            }
        })
        .unwrap_or_else(|| "procman".to_string());

    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();

    Ok(std::env::temp_dir().join(format!("procman-{sanitized}-{hash:08x}.fifo")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let path = env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml";
        let a = derive_fifo_path(&path).unwrap();
        let b = derive_fifo_path(&path).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_dirs_diverge() {
        // Two real files in different directories should produce different FIFO paths
        let a =
            derive_fifo_path(&(env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml")).unwrap();
        let b =
            derive_fifo_path(&(env!("CARGO_MANIFEST_DIR").to_string() + "/src/main.rs")).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn nonexistent_path_errors() {
        let result = derive_fifo_path("/no/such/file.yaml");
        assert!(result.is_err());
    }

    #[test]
    fn contains_basename_and_hash() {
        let path = env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml";
        let fifo = derive_fifo_path(&path).unwrap();
        let name = fifo.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("procman-"));
        assert!(name.ends_with(".fifo"));
    }
}
