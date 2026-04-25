#![allow(dead_code)]

use std::path::PathBuf;

use nils_test_support::fixtures::write_text;
use tempfile::TempDir;

pub struct RepoFixture {
    _temp: TempDir,
    pub root: PathBuf,
    pub rest_setup: PathBuf,
    pub gql_setup: PathBuf,
}

impl RepoFixture {
    pub fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let rest_setup = root.join("setup/rest");
        let gql_setup = root.join("setup/graphql");
        std::fs::create_dir_all(&rest_setup).expect("create setup/rest");
        std::fs::create_dir_all(&gql_setup).expect("create setup/graphql");
        std::fs::create_dir_all(root.join(".git")).expect("create .git");
        Self {
            _temp: temp,
            root,
            rest_setup,
            gql_setup,
        }
    }

    pub fn write_rest_endpoints(&self, contents: &str) -> PathBuf {
        write_text(&self.rest_setup.join("endpoints.env"), contents)
    }

    pub fn write_rest_tokens(&self, contents: &str) -> PathBuf {
        write_text(&self.rest_setup.join("tokens.env"), contents)
    }

    pub fn write_gql_endpoints(&self, contents: &str) -> PathBuf {
        write_text(&self.gql_setup.join("endpoints.env"), contents)
    }

    pub fn write_gql_jwts(&self, contents: &str) -> PathBuf {
        write_text(&self.gql_setup.join("jwts.env"), contents)
    }

    pub fn write_gql_schema_env(&self, contents: &str) -> PathBuf {
        write_text(&self.gql_setup.join("schema.env"), contents)
    }

    pub fn write_gql_schema_file(&self, name: &str, contents: &str) -> PathBuf {
        write_text(&self.gql_setup.join(name), contents)
    }

    pub fn write_operation(&self, rel: &str, contents: &str) -> PathBuf {
        write_text(&self.root.join(rel), contents)
    }
}

impl Default for RepoFixture {
    fn default() -> Self {
        Self::new()
    }
}
