use crate::defs::index::DefIndex;
use crate::util;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const CACHE_FORMAT_VERSION: u8 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct CacheEnvelope {
    version: u8,
    source_roots: Vec<String>,
    index: DefIndex,
}

#[derive(Debug, Serialize)]
struct CacheEnvelopeRef<'a> {
    version: u8,
    source_roots: &'a [String],
    index: &'a DefIndex,
}

pub fn load_or_build() -> Result<DefIndex> {
    if !util::env_is_true("FZF_DEF_DOC_CACHE_ENABLED") {
        return super::index::build_index();
    }

    let cache_dir = util::zsh_cache_dir()?;
    let ts_file = cache_dir.join("fzf-def-doc.timestamp");
    let data_file = cache_dir.join("fzf-def-doc.cache.json");

    let ttl_minutes = util::env_or_default("FZF_DEF_DOC_CACHE_EXPIRE_MINUTES", "10")
        .parse::<i64>()
        .unwrap_or(10)
        .max(0);
    let ttl_seconds = ttl_minutes * 60;
    let source_roots = super::index::source_roots_identity()?;

    let now = util::now_epoch_seconds();
    if let Ok(raw) = fs::read_to_string(&ts_file)
        && let Ok(last) = raw.trim().parse::<i64>()
        && last > 0
        && (now - last) <= ttl_seconds
        && let Ok(data) = fs::read_to_string(&data_file)
        && let Ok(cache) = serde_json::from_str::<CacheEnvelope>(&data)
        && cache.version == CACHE_FORMAT_VERSION
        && cache.source_roots == source_roots
    {
        return Ok(cache.index);
    }

    let index = super::index::build_index()?;
    write_cache(&cache_dir, &ts_file, &data_file, &source_roots, &index, now)?;
    Ok(index)
}

fn write_cache(
    cache_dir: &PathBuf,
    ts_file: &PathBuf,
    data_file: &PathBuf,
    source_roots: &[String],
    index: &DefIndex,
    now: i64,
) -> Result<()> {
    let _ = fs::create_dir_all(cache_dir);
    let data = serde_json::to_string(&CacheEnvelopeRef {
        version: CACHE_FORMAT_VERSION,
        source_roots,
        index,
    })
    .context("serialize def index")?;
    let _ = fs::write(data_file, data);
    let _ = fs::write(ts_file, now.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::index::{AliasDef, DefIndex};
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn write(path: &std::path::Path, contents: &str) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn cache_disabled_builds_index_from_zsh_root() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write(&root.join(".zshrc"), "alias ll='ls -la'\n");

        let _guard = EnvGuard::set(&lock, "ZDOTDIR", root.to_string_lossy().as_ref());
        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "0");

        let index = load_or_build().expect("load");
        assert!(index.aliases.iter().any(|a| a.name == "ll"));
    }

    #[test]
    fn cache_enabled_uses_fresh_cache() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "1");
        let _guard_ttl = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_EXPIRE_MINUTES", "10");
        let _guard_cache_dir =
            EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_dir.to_string_lossy().as_ref());
        let _guard_zdot = EnvGuard::set(&lock, "ZDOTDIR", temp.path().to_string_lossy().as_ref());
        let _guard_extra = EnvGuard::remove(&lock, "FZF_DEF_EXTRA_ROOTS");

        let now = util::now_epoch_seconds();
        write(&cache_dir.join("fzf-def-doc.timestamp"), &now.to_string());
        let cached = DefIndex {
            aliases: vec![AliasDef {
                name: "cached".to_string(),
                value: "ls".to_string(),
                doc: None,
                source_file: "/tmp/.zshrc".to_string(),
            }],
            functions: Vec::new(),
        };
        write(
            &cache_dir.join("fzf-def-doc.cache.json"),
            &serde_json::to_string(&CacheEnvelope {
                version: CACHE_FORMAT_VERSION,
                source_roots: super::super::index::source_roots_identity().unwrap(),
                index: cached,
            })
            .unwrap(),
        );

        let index = load_or_build().expect("load");
        assert_eq!(index.aliases.len(), 1);
        assert_eq!(index.aliases[0].name, "cached");
    }

    #[test]
    fn cache_stale_rebuilds_and_overwrites() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("zsh");
        std::fs::create_dir_all(&root).unwrap();
        write(&root.join(".zshrc"), "alias fresh='echo hi'\n");

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let now = util::now_epoch_seconds();
        write(
            &cache_dir.join("fzf-def-doc.timestamp"),
            &(now - 3600).to_string(),
        );
        let stale = DefIndex {
            aliases: vec![AliasDef {
                name: "stale".to_string(),
                value: "bad".to_string(),
                doc: None,
                source_file: "/tmp/.zshrc".to_string(),
            }],
            functions: Vec::new(),
        };
        write(
            &cache_dir.join("fzf-def-doc.cache.json"),
            &serde_json::to_string(&stale).unwrap(),
        );

        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "1");
        let _guard_ttl = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_EXPIRE_MINUTES", "0");
        let _guard_cache_dir =
            EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_dir.to_string_lossy().as_ref());
        let _guard_zdot = EnvGuard::set(&lock, "ZDOTDIR", root.to_string_lossy().as_ref());

        let index = load_or_build().expect("load");
        assert!(index.aliases.iter().any(|a| a.name == "fresh"));
        assert!(!index.aliases.iter().any(|a| a.name == "stale"));

        let updated = std::fs::read_to_string(cache_dir.join("fzf-def-doc.cache.json")).unwrap();
        assert!(updated.contains("\"fresh\""));
    }

    #[test]
    fn cache_rebuilds_when_extra_roots_change() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("zsh");
        let extra_a = temp.path().join("extra-a");
        let extra_b = temp.path().join("extra-b");
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        write(&root.join(".zshrc"), "alias base='echo base'\n");
        write(&extra_a.join("a.zsh"), "alias extra-a='echo a'\n");
        write(&extra_b.join("b.zsh"), "alias extra-b='echo b'\n");

        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "1");
        let _guard_ttl = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_EXPIRE_MINUTES", "10");
        let _guard_cache_dir =
            EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_dir.to_string_lossy().as_ref());
        let _guard_zdot = EnvGuard::set(&lock, "ZDOTDIR", root.to_string_lossy().as_ref());

        {
            let _guard_extra = EnvGuard::set(
                &lock,
                "FZF_DEF_EXTRA_ROOTS",
                extra_a.to_string_lossy().as_ref(),
            );
            let index = load_or_build().expect("build from first roots");
            assert!(index.aliases.iter().any(|alias| alias.name == "extra-a"));
        }

        {
            let _guard_extra = EnvGuard::set(
                &lock,
                "FZF_DEF_EXTRA_ROOTS",
                extra_b.to_string_lossy().as_ref(),
            );
            let index = load_or_build().expect("rebuild from changed roots");
            assert!(index.aliases.iter().any(|alias| alias.name == "extra-b"));
            assert!(!index.aliases.iter().any(|alias| alias.name == "extra-a"));
        }
    }

    #[test]
    fn cache_rebuilds_when_logical_root_path_changes() {
        let lock = GlobalStateLock::new();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("zsh");
        let extra = temp.path().join("extra");
        let extra_link = temp.path().join("extra-link");
        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        write(&root.join(".zshrc"), "alias base='echo base'\n");
        write(&extra.join("private.zsh"), "alias private='echo private'\n");
        std::os::unix::fs::symlink(&extra, &extra_link).expect("symlink");

        let _guard_cache = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_ENABLED", "1");
        let _guard_ttl = EnvGuard::set(&lock, "FZF_DEF_DOC_CACHE_EXPIRE_MINUTES", "10");
        let _guard_cache_dir =
            EnvGuard::set(&lock, "ZSH_CACHE_DIR", cache_dir.to_string_lossy().as_ref());
        let _guard_zdot = EnvGuard::set(&lock, "ZDOTDIR", root.to_string_lossy().as_ref());

        {
            let _guard_extra = EnvGuard::set(
                &lock,
                "FZF_DEF_EXTRA_ROOTS",
                extra_link.to_string_lossy().as_ref(),
            );
            let index = load_or_build().expect("build through logical root");
            let source = &index
                .aliases
                .iter()
                .find(|alias| alias.name == "private")
                .expect("private alias")
                .source_file;
            assert!(source.starts_with(extra_link.to_string_lossy().as_ref()));
        }

        std::fs::remove_file(&extra_link).expect("remove symlink");

        {
            let _guard_extra = EnvGuard::set(
                &lock,
                "FZF_DEF_EXTRA_ROOTS",
                extra.to_string_lossy().as_ref(),
            );
            let index = load_or_build().expect("rebuild through physical root");
            let source = &index
                .aliases
                .iter()
                .find(|alias| alias.name == "private")
                .expect("private alias")
                .source_file;
            assert!(source.starts_with(extra.to_string_lossy().as_ref()));
            assert!(std::path::Path::new(source).exists());
        }
    }
}
