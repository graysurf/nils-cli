pub const GIT_DESCRIBE: &str = env!("NILS_GIT_DESCRIBE");
pub const RUSTC_VERSION: &str = env!("NILS_RUSTC_VERSION");

pub fn long_version(pkg_version: &str) -> String {
    format!("{pkg_version} ({GIT_DESCRIBE}, rustc {RUSTC_VERSION})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_version_preserves_package_version() {
        let version = long_version("9.9.9");

        assert!(version.starts_with("9.9.9 ("));
        assert!(version.contains(GIT_DESCRIBE));
        assert!(version.contains("rustc "));
        assert!(version.ends_with(')'));
    }

    #[test]
    fn build_metadata_consts_are_not_empty() {
        assert!(!GIT_DESCRIBE.is_empty());
        assert!(!RUSTC_VERSION.is_empty());
    }
}
