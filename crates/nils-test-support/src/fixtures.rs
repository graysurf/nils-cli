use std::path::{Path, PathBuf};

use crate::git::{InitRepoOptions, commit_file, git, init_repo_at_with};

const COMPLETE_TEST_FIRST_EVIDENCE: &str = r#"{"schema_version":"test-first-evidence.record.v2","change_classification":"behavior-change","contract_delta":{"changed_behaviors":["durable gate"]},"no_existing_tests_reason":"fixture has no existing tests","waiver":{"reason":"fixture","kind":"non-testable","why_no_red":"fixture path","substitute_validation":["cargo test"]},"final_validations":[{"command":"cargo test","status":"pass","scope":"focused"}],"no_residual_gaps":true}"#;

pub struct SubjectBoundEvidenceFixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub repo: PathBuf,
    pub evidence: PathBuf,
}

impl SubjectBoundEvidenceFixture {
    /// Creates a git repository plus a complete evidence record and delegates
    /// the baseline/delivery subject binding to the consuming crate.
    pub fn new(
        remote: &str,
        baseline_files: &[(&str, &str)],
        bind: impl FnMut(&str, &Path, &Path) -> i32,
    ) -> Self {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let repo = init_subject_repo(&root, "repo", remote);
        for (path, contents) in baseline_files {
            write_text(&repo.join(path), contents);
        }
        if !baseline_files.is_empty() {
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-m", "configure baseline"]);
        }
        let evidence = root.join("evidence");
        bind_complete_test_first_evidence(&repo, &evidence, bind);
        Self {
            _temp: temp,
            root,
            repo,
            evidence,
        }
    }
}

/// Creates a deterministic git repository with one initial commit and remote.
pub fn init_subject_repo(root: &Path, name: &str, remote: &str) -> PathBuf {
    let repo = root.join(name);
    std::fs::create_dir_all(&repo).expect("create subject repo");
    init_repo_at_with(
        &repo,
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    git(&repo, &["remote", "add", "origin", remote]);
    repo
}

/// Owns the canonical complete evidence fixture and its baseline/delivery
/// lifecycle while the caller supplies the product-specific binding command.
pub fn bind_complete_test_first_evidence(
    repo: &Path,
    evidence: &Path,
    mut bind: impl FnMut(&str, &Path, &Path) -> i32,
) {
    std::fs::create_dir_all(evidence).expect("create evidence dir");
    write_text(
        &evidence.join("test-first-evidence.json"),
        COMPLETE_TEST_FIRST_EVIDENCE,
    );
    assert_eq!(bind("bind-baseline", repo, evidence), 0);
    commit_file(repo, "delivery.txt", "delivery\n", "delivery");
    assert_eq!(bind("bind-delivery", repo, evidence), 0);
}

pub struct RestSetupFixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub setup_dir: PathBuf,
}

impl RestSetupFixture {
    /// Creates a temp dir with `setup/rest/` and returns paths.
    pub fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let setup_dir = root.join("setup/rest");
        std::fs::create_dir_all(&setup_dir).expect("create setup/rest");
        Self {
            _temp: temp,
            root,
            setup_dir,
        }
    }

    /// Writes setup/rest/endpoints.env (base file; overridden by endpoints.local.env).
    pub fn write_endpoints_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("endpoints.env"), contents)
    }

    /// Writes setup/rest/endpoints.local.env (overrides endpoints.env).
    pub fn write_endpoints_local_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("endpoints.local.env"), contents)
    }

    /// Writes setup/rest/tokens.env (base file; overridden by tokens.local.env).
    pub fn write_tokens_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("tokens.env"), contents)
    }

    /// Writes setup/rest/tokens.local.env (overrides tokens.env).
    pub fn write_tokens_local_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("tokens.local.env"), contents)
    }
}

impl Default for RestSetupFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub struct GraphqlSetupFixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub setup_dir: PathBuf,
}

impl GraphqlSetupFixture {
    /// Creates a temp dir with `setup/graphql/` and returns paths.
    pub fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let setup_dir = root.join("setup/graphql");
        std::fs::create_dir_all(&setup_dir).expect("create setup/graphql");
        Self {
            _temp: temp,
            root,
            setup_dir,
        }
    }

    /// Writes setup/graphql/endpoints.env (base file; overridden by endpoints.local.env).
    pub fn write_endpoints_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("endpoints.env"), contents)
    }

    /// Writes setup/graphql/endpoints.local.env (overrides endpoints.env).
    pub fn write_endpoints_local_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("endpoints.local.env"), contents)
    }

    /// Writes setup/graphql/jwts.env (base file; overridden by jwts.local.env).
    pub fn write_jwts_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("jwts.env"), contents)
    }

    /// Writes setup/graphql/jwts.local.env (overrides jwts.env).
    pub fn write_jwts_local_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("jwts.local.env"), contents)
    }

    /// Writes setup/graphql/schema.env (points to a schema file).
    pub fn write_schema_env(&self, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join("schema.env"), contents)
    }

    /// Writes a schema file under setup/graphql (use with schema.env).
    pub fn write_schema_file(&self, name: &str, contents: &str) -> PathBuf {
        write_text(&self.setup_dir.join(name), contents)
    }
}

impl Default for GraphqlSetupFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SuiteFixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub suite_path: PathBuf,
}

impl SuiteFixture {
    /// Creates a temp dir for a suite.json and related files.
    pub fn new() -> Self {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let root = temp.path().to_path_buf();
        let suite_path = root.join("suite.json");
        Self {
            _temp: temp,
            root,
            suite_path,
        }
    }

    /// Writes a minimal REST suite manifest and request file.
    pub fn write_minimal_rest_suite(&self, case_id: &str, request_rel: &str) -> PathBuf {
        let request_path = self.root.join(request_rel);
        write_text(
            &request_path,
            r#"{"method":"GET","path":"/health","expect":{"status":200}}"#,
        );
        let manifest = format!(
            r#"{{
  "version": 1,
  "cases": [
    {{
      "id": "{case_id}",
      "type": "rest",
      "request": "{request_rel}"
    }}
  ]
}}"#
        );
        write_text(&self.suite_path, &manifest)
    }

    /// Writes a minimal GraphQL suite manifest and operation file.
    pub fn write_minimal_graphql_suite(&self, case_id: &str, op_rel: &str) -> PathBuf {
        let op_path = self.root.join(op_rel);
        write_text(&op_path, "query Health { __typename }\n");
        let manifest = format!(
            r#"{{
  "version": 1,
  "cases": [
    {{
      "id": "{case_id}",
      "type": "graphql",
      "op": "{op_rel}"
    }}
  ]
}}"#
        );
        write_text(&self.suite_path, &manifest)
    }
}

impl Default for SuiteFixture {
    fn default() -> Self {
        Self::new()
    }
}

pub fn write_text(path: &Path, contents: &str) -> PathBuf {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, contents).expect("write fixture");
    path.to_path_buf()
}
