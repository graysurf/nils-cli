use std::path::{Path, PathBuf};

use anyhow::Context;
use nils_common::env as shared_env;

use crate::{Result, cli_util};

pub fn find_repo_root(start_dir: &Path) -> Result<PathBuf> {
    let mut dir = start_dir;
    loop {
        if dir.join(".git").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => anyhow::bail!("Must run inside a git work tree"),
        }
    }
}

pub fn resolve_path_from_repo_root(repo_root: &Path, raw: &str) -> PathBuf {
    let raw = raw.trim();
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

pub fn resolve_rest_base_url_for_env(setup_dir: &Path, env_value: &str) -> Result<String> {
    let env_value = env_value.trim();
    if env_value.starts_with("http://") || env_value.starts_with("https://") {
        return Ok(env_value.to_string());
    }

    let endpoints_env = setup_dir.join("endpoints.env");
    let endpoints_local = setup_dir.join("endpoints.local.env");
    let endpoints_files: Vec<&Path> = if endpoints_env.is_file() {
        vec![&endpoints_env, &endpoints_local]
    } else {
        Vec::new()
    };
    if endpoints_files.is_empty() {
        anyhow::bail!("endpoints.env not found (expected under setup/rest/)");
    }

    let env_key = crate::env_file::normalize_env_key(env_value);
    let key = format!("REST_URL_{env_key}");
    let found = crate::env_file::read_var_last_wins(&key, &endpoints_files)?;
    let Some(found) = found else {
        let mut available = cli_util::list_available_suffixes(&endpoints_env, "REST_URL_");
        if endpoints_local.is_file() {
            available.extend(cli_util::list_available_suffixes(
                &endpoints_local,
                "REST_URL_",
            ));
            available.sort();
            available.dedup();
        }
        let available = if available.is_empty() {
            "none".to_string()
        } else {
            available.join(" ")
        };
        anyhow::bail!("Unknown env '{env_value}' (available: {available})");
    };

    Ok(found)
}

pub fn resolve_gql_url_for_env(setup_dir: &Path, env_value: &str) -> Result<String> {
    let env_value = env_value.trim();
    if env_value.starts_with("http://") || env_value.starts_with("https://") {
        return Ok(env_value.to_string());
    }

    let endpoints_env = setup_dir.join("endpoints.env");
    let endpoints_local = setup_dir.join("endpoints.local.env");
    let endpoints_files: Vec<&Path> = if endpoints_env.is_file() {
        vec![&endpoints_env, &endpoints_local]
    } else {
        Vec::new()
    };
    if endpoints_files.is_empty() {
        anyhow::bail!("endpoints.env not found (expected under setup/graphql/)");
    }

    let env_key = crate::env_file::normalize_env_key(env_value);
    let key = format!("GQL_URL_{env_key}");
    let found = crate::env_file::read_var_last_wins(&key, &endpoints_files)?;
    let Some(found) = found else {
        let mut available = cli_util::list_available_suffixes(&endpoints_env, "GQL_URL_");
        if endpoints_local.is_file() {
            available.extend(cli_util::list_available_suffixes(
                &endpoints_local,
                "GQL_URL_",
            ));
            available.sort();
            available.dedup();
        }
        let available = if available.is_empty() {
            "none".to_string()
        } else {
            available.join(" ")
        };
        anyhow::bail!("Unknown env '{env_value}' (available: {available})");
    };

    Ok(found)
}

pub fn resolve_grpc_url_for_env(setup_dir: &Path, env_value: &str) -> Result<String> {
    let env_value = env_value.trim();
    if env_value.starts_with("http://") || env_value.starts_with("https://") {
        return Ok(env_value.to_string());
    }

    let endpoints_env = setup_dir.join("endpoints.env");
    let endpoints_local = setup_dir.join("endpoints.local.env");
    let endpoints_files: Vec<&Path> = if endpoints_env.is_file() {
        vec![&endpoints_env, &endpoints_local]
    } else {
        Vec::new()
    };
    if endpoints_files.is_empty() {
        anyhow::bail!("endpoints.env not found (expected under setup/grpc/)");
    }

    let env_key = crate::env_file::normalize_env_key(env_value);
    let key = format!("GRPC_URL_{env_key}");
    let found = crate::env_file::read_var_last_wins(&key, &endpoints_files)?;
    let Some(found) = found else {
        let mut available = cli_util::list_available_suffixes(&endpoints_env, "GRPC_URL_");
        if endpoints_local.is_file() {
            available.extend(cli_util::list_available_suffixes(
                &endpoints_local,
                "GRPC_URL_",
            ));
            available.sort();
            available.dedup();
        }
        let available = if available.is_empty() {
            "none".to_string()
        } else {
            available.join(" ")
        };
        anyhow::bail!("Unknown env '{env_value}' (available: {available})");
    };

    Ok(found)
}

pub fn resolve_ws_url_for_env(setup_dir: &Path, env_value: &str) -> Result<String> {
    let env_value = env_value.trim();
    if env_value.starts_with("ws://") || env_value.starts_with("wss://") {
        return Ok(env_value.to_string());
    }

    let endpoints_env = setup_dir.join("endpoints.env");
    let endpoints_local = setup_dir.join("endpoints.local.env");
    let endpoints_files: Vec<&Path> = if endpoints_env.is_file() {
        vec![&endpoints_env, &endpoints_local]
    } else {
        Vec::new()
    };
    if endpoints_files.is_empty() {
        anyhow::bail!("endpoints.env not found (expected under setup/websocket/)");
    }

    let env_key = crate::env_file::normalize_env_key(env_value);
    let key = format!("WS_URL_{env_key}");
    let found = crate::env_file::read_var_last_wins(&key, &endpoints_files)?;
    let Some(found) = found else {
        let mut available = cli_util::list_available_suffixes(&endpoints_env, "WS_URL_");
        if endpoints_local.is_file() {
            available.extend(cli_util::list_available_suffixes(
                &endpoints_local,
                "WS_URL_",
            ));
            available.sort();
            available.dedup();
        }
        let available = if available.is_empty() {
            "none".to_string()
        } else {
            available.join(" ")
        };
        anyhow::bail!("Unknown env '{env_value}' (available: {available})");
    };

    Ok(found)
}

#[derive(Debug, Clone)]
pub struct SuiteSelection {
    pub suite_key: String,
    pub suite_path: PathBuf,
}

fn normalize_suite_key(raw: &str) -> String {
    let key = raw.trim();
    let key = key.strip_suffix(".suite.json").unwrap_or(key);
    let key = key.strip_suffix(".json").unwrap_or(key);
    key.trim().to_string()
}

pub fn resolve_suite_selection(
    repo_root: &Path,
    suite: Option<&str>,
    suite_file: Option<&str>,
) -> Result<SuiteSelection> {
    if suite.is_some() && suite_file.is_some() {
        anyhow::bail!("Use only one of --suite or --suite-file");
    }
    let suite = suite.and_then(cli_util::trim_non_empty);
    let suite_file = suite_file.and_then(cli_util::trim_non_empty);
    if suite.is_none() && suite_file.is_none() {
        anyhow::bail!("Missing suite (use --suite or --suite-file)");
    }

    if let Some(suite_file) = suite_file {
        let suite_path = resolve_path_from_repo_root(repo_root, &suite_file);
        let suite_path = std::fs::canonicalize(&suite_path).unwrap_or(suite_path);
        if !suite_path.is_file() {
            anyhow::bail!("Suite file not found: {}", suite_path.to_string_lossy());
        }
        let suite_key = suite_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("suite")
            .to_string();
        return Ok(SuiteSelection {
            suite_key,
            suite_path,
        });
    }

    let suite_key = normalize_suite_key(&suite.unwrap_or_default());
    if suite_key.is_empty() {
        anyhow::bail!("Missing suite (use --suite or --suite-file)");
    }

    let suites_dir_override =
        shared_env::env_non_empty("API_TEST_SUITES_DIR").and_then(|s| cli_util::trim_non_empty(&s));

    let candidate = if let Some(dir) = suites_dir_override {
        let abs = resolve_path_from_repo_root(repo_root, &dir);
        abs.join(format!("{suite_key}.suite.json"))
    } else {
        let mut p = repo_root
            .join("tests/api/suites")
            .join(format!("{suite_key}.suite.json"));
        if !p.is_file() {
            p = repo_root
                .join("setup/api/suites")
                .join(format!("{suite_key}.suite.json"));
        }
        p
    };

    let suite_path = std::fs::canonicalize(&candidate).unwrap_or(candidate);
    if !suite_path.is_file() {
        anyhow::bail!("Suite file not found: {}", suite_path.to_string_lossy());
    }

    Ok(SuiteSelection {
        suite_key,
        suite_path,
    })
}

pub fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        anyhow::bail!("invalid path: {}", path.display());
    };
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create directory: {}", parent.display()))?;
    std::fs::write(path, contents).with_context(|| format!("write file: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nils_test_support::{EnvGuard, GlobalStateLock, tempdir::ScopedTempDir};

    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    #[test]
    fn suite_find_repo_root_success_and_failure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();

        let found = find_repo_root(&root.join("a/b/c")).unwrap();
        assert_eq!(found, root);

        let tmp2 = ScopedTempDir::outside_git_ancestry("api-testing-core-non-git-");
        let err = find_repo_root(tmp2.path()).unwrap_err();
        assert!(format!("{err:#}").contains("Must run inside a git work tree"));
    }

    #[test]
    fn suite_resolve_path_from_repo_root_handles_absolute_and_relative() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        assert_eq!(
            resolve_path_from_repo_root(root, " setup/rest "),
            root.join("setup/rest")
        );
        assert_eq!(
            resolve_path_from_repo_root(root, "/abs/path"),
            PathBuf::from("/abs/path")
        );
    }

    #[cfg(windows)]
    #[test]
    fn suite_resolve_path_from_repo_root_handles_windows_absolute_paths() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let drive_abs = r"C:\abs\path";
        assert_eq!(
            resolve_path_from_repo_root(root, drive_abs),
            PathBuf::from(drive_abs)
        );

        let unc_abs = r"\\server\share\suite.yaml";
        assert_eq!(
            resolve_path_from_repo_root(root, unc_abs),
            PathBuf::from(unc_abs)
        );
    }

    #[test]
    fn suite_resolve_rest_base_url_from_url_and_env_files() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/rest");

        assert_eq!(
            resolve_rest_base_url_for_env(&setup_dir, "https://example.test").unwrap(),
            "https://example.test"
        );

        let err = resolve_rest_base_url_for_env(&setup_dir, "prod").unwrap_err();
        assert!(format!("{err:#}").contains("endpoints.env not found"));
        assert!(format!("{err:#}").contains("setup/rest"));

        let endpoints_env = setup_dir.join("endpoints.env");
        let endpoints_local = setup_dir.join("endpoints.local.env");
        write(&endpoints_env, "REST_URL_PROD=http://base.test\n");
        write(
            &endpoints_local,
            "REST_URL_PROD=http://local.test\nREST_URL_LOCAL=http://x\n",
        );

        assert_eq!(
            resolve_rest_base_url_for_env(&setup_dir, "prod").unwrap(),
            "http://local.test"
        );

        let err = resolve_rest_base_url_for_env(&setup_dir, "nope").unwrap_err();
        assert!(format!("{err:#}").contains("Unknown env 'nope'"));
        assert!(format!("{err:#}").contains("available: local prod"));
    }

    #[test]
    fn suite_resolve_gql_url_from_url_and_env_files() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/graphql");

        assert_eq!(
            resolve_gql_url_for_env(&setup_dir, "http://example.test/graphql").unwrap(),
            "http://example.test/graphql"
        );

        let err = resolve_gql_url_for_env(&setup_dir, "prod").unwrap_err();
        assert!(format!("{err:#}").contains("endpoints.env not found"));
        assert!(format!("{err:#}").contains("setup/graphql"));

        let endpoints_env = setup_dir.join("endpoints.env");
        let endpoints_local = setup_dir.join("endpoints.local.env");
        write(&endpoints_env, "GQL_URL_PROD=http://base.test/graphql\n");
        write(
            &endpoints_local,
            "GQL_URL_PROD=http://local.test/graphql\nGQL_URL_LOCAL=http://x\n",
        );

        assert_eq!(
            resolve_gql_url_for_env(&setup_dir, "prod").unwrap(),
            "http://local.test/graphql"
        );

        let err = resolve_gql_url_for_env(&setup_dir, "nope").unwrap_err();
        assert!(format!("{err:#}").contains("Unknown env 'nope'"));
        assert!(format!("{err:#}").contains("available: local prod"));
    }

    #[test]
    fn suite_resolve_grpc_url_from_url_and_env_files() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/grpc");

        assert_eq!(
            resolve_grpc_url_for_env(&setup_dir, "https://grpc.test:8443").unwrap(),
            "https://grpc.test:8443"
        );

        let err = resolve_grpc_url_for_env(&setup_dir, "prod").unwrap_err();
        assert!(format!("{err:#}").contains("endpoints.env not found"));
        assert!(format!("{err:#}").contains("setup/grpc"));

        let endpoints_env = setup_dir.join("endpoints.env");
        let endpoints_local = setup_dir.join("endpoints.local.env");
        write(&endpoints_env, "GRPC_URL_PROD=grpc.prod:443\n");
        write(
            &endpoints_local,
            "GRPC_URL_PROD=grpc.local:443\nGRPC_URL_LOCAL=127.0.0.1:50051\n",
        );

        assert_eq!(
            resolve_grpc_url_for_env(&setup_dir, "prod").unwrap(),
            "grpc.local:443"
        );

        let err = resolve_grpc_url_for_env(&setup_dir, "nope").unwrap_err();
        assert!(format!("{err:#}").contains("Unknown env 'nope'"));
        assert!(format!("{err:#}").contains("available: local prod"));
    }

    #[test]
    fn suite_resolve_ws_url_from_url_and_env_files() {
        let tmp = TempDir::new().unwrap();
        let setup_dir = tmp.path().join("setup/websocket");

        assert_eq!(
            resolve_ws_url_for_env(&setup_dir, "wss://socket.test/ws").unwrap(),
            "wss://socket.test/ws"
        );

        let err = resolve_ws_url_for_env(&setup_dir, "prod").unwrap_err();
        assert!(format!("{err:#}").contains("endpoints.env not found"));
        assert!(format!("{err:#}").contains("setup/websocket"));

        let endpoints_env = setup_dir.join("endpoints.env");
        let endpoints_local = setup_dir.join("endpoints.local.env");
        write(&endpoints_env, "WS_URL_PROD=ws://socket.prod/ws\n");
        write(
            &endpoints_local,
            "WS_URL_PROD=ws://socket.local/ws\nWS_URL_LOCAL=ws://127.0.0.1:9001/ws\n",
        );

        assert_eq!(
            resolve_ws_url_for_env(&setup_dir, "prod").unwrap(),
            "ws://socket.local/ws"
        );

        let err = resolve_ws_url_for_env(&setup_dir, "nope").unwrap_err();
        assert!(format!("{err:#}").contains("Unknown env 'nope'"));
        assert!(format!("{err:#}").contains("available: local prod"));
    }

    #[test]
    fn suite_write_file_creates_parent_dirs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a/b/c.txt");
        write_file(&path, b"hello").unwrap();
        assert!(path.is_file());
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn suite_resolve_resolves_suite_name_under_tests_dir() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::remove(&lock, "API_TEST_SUITES_DIR");

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("tests/api/suites")).unwrap();
        std::fs::write(
            root.join("tests/api/suites/smoke.suite.json"),
            br#"{"version":1,"cases":[]}"#,
        )
        .unwrap();

        let sel = resolve_suite_selection(root, Some("smoke"), None).unwrap();
        assert!(
            sel.suite_path
                .ends_with("tests/api/suites/smoke.suite.json")
        );
    }

    #[test]
    fn suite_resolve_resolves_suite_name_under_setup_dir() {
        let lock = GlobalStateLock::new();
        let _guard = EnvGuard::remove(&lock, "API_TEST_SUITES_DIR");

        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("setup/api/suites")).unwrap();
        std::fs::write(
            root.join("setup/api/suites/smoke.suite.json"),
            br#"{"version":1,"cases":[]}"#,
        )
        .unwrap();

        let sel = resolve_suite_selection(root, Some("smoke"), None).unwrap();
        assert!(
            sel.suite_path
                .ends_with("setup/api/suites/smoke.suite.json")
        );
    }

    #[test]
    fn suite_resolve_rejects_invalid_suite_args() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let err = resolve_suite_selection(root, Some("a"), Some("b")).unwrap_err();
        assert!(format!("{err:#}").contains("Use only one of --suite or --suite-file"));

        let err = resolve_suite_selection(root, None, None).unwrap_err();
        assert!(format!("{err:#}").contains("Missing suite (use --suite or --suite-file)"));
    }

    #[test]
    fn suite_resolve_resolves_suite_file_relative_to_repo_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("setup/api/suites")).unwrap();
        std::fs::write(
            root.join("setup/api/suites/smoke.suite.json"),
            br#"{"version":1,"cases":[]}"#,
        )
        .unwrap();

        let sel =
            resolve_suite_selection(root, None, Some("setup/api/suites/smoke.suite.json")).unwrap();
        assert_eq!(sel.suite_key, "smoke.suite.json");
        assert!(
            sel.suite_path
                .ends_with("setup/api/suites/smoke.suite.json")
        );
    }
}
