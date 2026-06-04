pub mod auto_refresh;
pub mod current;
pub mod login;
pub mod output;
pub mod refresh;
pub mod remove;
pub mod save;
pub mod status;
pub mod sync;
pub mod use_secret;

use anyhow::Result;
use std::path::Path;

pub fn identity_from_auth_file(path: &Path) -> Result<Option<String>> {
    crate::runtime::auth::identity_from_auth_file(path).map_err(anyhow::Error::from)
}

pub fn email_from_auth_file(path: &Path) -> Result<Option<String>> {
    crate::runtime::auth::email_from_auth_file(path).map_err(anyhow::Error::from)
}

pub fn account_id_from_auth_file(path: &Path) -> Result<Option<String>> {
    crate::runtime::auth::account_id_from_auth_file(path).map_err(anyhow::Error::from)
}

pub fn last_refresh_from_auth_file(path: &Path) -> Result<Option<String>> {
    crate::runtime::auth::last_refresh_from_auth_file(path).map_err(anyhow::Error::from)
}

pub fn identity_key_from_auth_file(path: &Path) -> Result<Option<String>> {
    crate::runtime::auth::identity_key_from_auth_file(path).map_err(anyhow::Error::from)
}

pub fn is_invalid_secret_target(target: &str) -> bool {
    target.contains('/') || target.contains('\\') || target.contains("..")
}

pub fn normalize_secret_file_name(target: &str) -> String {
    if target.ends_with(".json") {
        return target.to_string();
    }
    format!("{target}.json")
}

#[cfg(test)]
mod tests {
    use super::{is_invalid_secret_target, normalize_secret_file_name};

    #[test]
    fn secret_target_validation_rejects_paths_and_traversal() {
        assert!(is_invalid_secret_target("../a.json"));
        assert!(is_invalid_secret_target("a/b.json"));
        assert!(is_invalid_secret_target(r"a\b.json"));
        assert!(!is_invalid_secret_target("alpha"));
        assert!(!is_invalid_secret_target("alpha.json"));
    }

    #[test]
    fn normalize_secret_file_name_appends_json_suffix_only_once() {
        assert_eq!(normalize_secret_file_name("alpha"), "alpha.json");
        assert_eq!(normalize_secret_file_name("alpha.json"), "alpha.json");
    }
}
