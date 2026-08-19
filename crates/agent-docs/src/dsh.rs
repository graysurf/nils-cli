//! Additive helpers for the isolated DeepSeek Harness catalog view.
//!
//! DSH is a catalog tag and runtime integration boundary, not a variant of the
//! stable public [`crate::model::Product`] enum.

use std::path::Path;

use crate::config::load_dsh_catalog_from_roots;
use crate::env::ResolvedRoots;
use crate::model::{ConfigLoadError, Context, FallbackMode, Phase, ValidationContract};
use crate::resolver::all_validation_contracts_for_product;

/// Load the current DSH catalog projection and return its validation contracts.
///
/// Unscoped and `dsh`-tagged contracts are included. Contracts tagged only for
/// a stable product are excluded. The complete source catalog is validated
/// before projection, so malformed excluded entries still fail closed.
pub fn validation_contracts_from_roots(
    roots: &ResolvedRoots,
) -> Result<Vec<ValidationContract>, ConfigLoadError> {
    let catalog = load_dsh_catalog_from_roots(roots)?;
    Ok(all_validation_contracts_for_product(
        roots,
        None,
        catalog.as_loaded(),
    ))
}

/// Return whether one exact DSH session already holds a current activation.
///
/// This isolated verifier reads records produced by `session context`; it does
/// not create or refresh activation state. Any missing, malformed, stale, or
/// mismatched record fails closed as `false`.
pub fn session_intent_is_current(
    roots: &ResolvedRoots,
    session_id: &str,
    state_home: &Path,
    intent: &Context,
    phase: Option<&Phase>,
    fallback: FallbackMode,
) -> bool {
    crate::session::dsh_session_intent_is_current(
        roots, session_id, state_home, intent, phase, fallback,
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn validation_helper_returns_unscoped_and_dsh_contracts_only() {
        let temp = TempDir::new().expect("tempdir");
        let docs_home = temp.path().join("home");
        let project = temp.path().join("project");
        fs::create_dir_all(&docs_home).expect("docs home");
        fs::create_dir_all(&project).expect("project");
        fs::write(
            project.join("AGENT_DOCS.toml"),
            r#"
[[validation]]
context = "unscoped"
commands = ["unscoped-check"]

[[validation]]
context = "dsh-only"
commands = ["dsh-check"]
product = "dsh"

[[validation]]
context = "codex-only"
commands = ["codex-check"]
product = "codex"
"#,
        )
        .expect("catalog");
        let roots = ResolvedRoots::for_paths(docs_home, project);

        let contracts = validation_contracts_from_roots(&roots).expect("DSH contracts");
        assert_eq!(
            contracts
                .iter()
                .map(|contract| contract.context.as_str())
                .collect::<Vec<_>>(),
            vec!["unscoped", "dsh-only"]
        );
        assert_eq!(contracts[1].commands, vec!["dsh-check"]);
    }

    #[test]
    fn session_intent_verification_accepts_only_a_current_dsh_context_record() {
        let temp = TempDir::new().expect("tempdir");
        let docs_home = temp.path().join("home");
        let project = temp.path().join("project");
        let state_home = temp.path().join("state");
        fs::create_dir_all(&docs_home).expect("docs home");
        fs::create_dir_all(&project).expect("project");
        fs::write(docs_home.join("EDIT.md"), "current policy\n").expect("policy doc");
        fs::write(
            docs_home.join("AGENT_DOCS.toml"),
            r#"
[[document]]
context = "project-dev"
scope = "home"
path = "EDIT.md"
required = true
phase = "edit"
"#,
        )
        .expect("catalog");
        let code = crate::run_with_args([
            "agent-docs",
            "--docs-home",
            docs_home.to_str().expect("docs UTF-8"),
            "--project-path",
            project.to_str().expect("project UTF-8"),
            "session",
            "context",
            "--session-id",
            "dsh-session",
            "--product",
            "dsh",
            "--state-home",
            state_home.to_str().expect("state UTF-8"),
            "--intent",
            "project-dev",
            "--phase",
            "edit",
            "--request-id",
            "context:test",
            "--format",
            "json",
        ]);
        assert_eq!(code, 0);

        let roots = crate::env::resolve_roots(&crate::env::PathOverrides {
            docs_home: Some(docs_home.clone()),
            project_path: Some(project),
        })
        .expect("resolved roots");
        let intent = crate::model::Context::parse("project-dev").expect("intent");
        let phase = crate::model::Phase::parse("edit").expect("phase");
        assert!(session_intent_is_current(
            &roots,
            "dsh-session",
            &state_home,
            &intent,
            Some(&phase),
            crate::model::FallbackMode::Auto,
        ));

        fs::write(docs_home.join("EDIT.md"), "changed policy\n").expect("mutate policy");
        assert!(!session_intent_is_current(
            &roots,
            "dsh-session",
            &state_home,
            &intent,
            Some(&phase),
            crate::model::FallbackMode::Auto,
        ));
    }
}
