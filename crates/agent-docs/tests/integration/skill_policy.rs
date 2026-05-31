//! Phase 2 — opt-in `[skills]` policy. `audit` flags skill directories whose
//! names do not match the required `^(project|private)-` prefix, but only when
//! the project catalog opts in via `enforce_name_prefix = true`. Repos that do
//! not declare `[skills]` keep their previous, unchanged audit behavior.

use super::common::TestEnv;
use serde_json::Value;

const EXIT_OK: i32 = 0;
const EXIT_STRICT: i32 = 1;
const EXIT_CONFIG: i32 = 3;

/// Extract `(name, ok)` pairs from an audit JSON report's `skills` array.
fn skill_results(json: &Value) -> Vec<(String, bool)> {
    json["skills"]
        .as_array()
        .expect("skills array in audit JSON")
        .iter()
        .map(|check| {
            (
                check["name"].as_str().expect("skill name").to_string(),
                check["ok"].as_bool().expect("skill ok"),
            )
        })
        .collect()
}

#[test]
fn audit_flags_unprefixed_skill_when_opted_in() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/project-good/SKILL.md", "ok\n");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    assert_eq!(out.code, EXIT_OK, "stderr: {}", out.stderr);
    let json = out.json();
    // Results are sorted by directory name for deterministic output.
    assert_eq!(
        skill_results(&json),
        vec![
            ("helper-bad".to_string(), false),
            ("project-good".to_string(), true),
        ],
        "stdout: {}",
        out.stdout
    );
    assert_eq!(json["problems"].as_u64(), Some(1));
}

#[test]
fn audit_strict_fails_on_unprefixed_skill() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&[
        "audit", "--target", "project", "--strict", "--format", "json",
    ]);
    assert_eq!(out.code, EXIT_STRICT, "stderr: {}", out.stderr);
}

#[test]
fn audit_passes_when_all_skills_prefixed() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/project-one/SKILL.md", "ok\n");
    env.write_project_doc(".agents/skills/private-two/SKILL.md", "ok\n");

    let out = env.run(&[
        "audit", "--target", "project", "--strict", "--format", "json",
    ]);
    assert_eq!(out.code, EXIT_OK, "stderr: {}", out.stderr);
    let json = out.json();
    assert!(skill_results(&json).iter().all(|(_, ok)| *ok));
    assert_eq!(json["problems"].as_u64(), Some(0));
}

#[test]
fn audit_ignores_skills_without_optin() {
    let env = TestEnv::new();
    // No `[skills]` table: the project does not participate.
    env.write_project_catalog("");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    assert_eq!(out.code, EXIT_OK, "stderr: {}", out.stderr);
    let json = out.json();
    assert!(
        json["skills"].as_array().expect("skills array").is_empty(),
        "stdout: {}",
        out.stdout
    );
    assert_eq!(json["problems"].as_u64(), Some(0));
}

#[test]
fn audit_optout_disables_checks() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = false\n");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    assert_eq!(out.code, EXIT_OK, "stderr: {}", out.stderr);
    assert!(
        out.json()["skills"]
            .as_array()
            .expect("skills array")
            .is_empty()
    );
}

#[test]
fn audit_home_target_skips_project_skill_scan() {
    // Skills are a project concern; a Home-scoped audit must not scan them even
    // when the project opted in.
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "home", "--format", "json"]);
    let json = out.json();
    assert!(
        json["skills"].as_array().expect("skills array").is_empty(),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn audit_honors_custom_prefixes_and_dir() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[skills]\nenforce_name_prefix = true\nallowed_prefixes = [\"board\"]\ndir = \"custom/skills\"\n",
    );
    env.write_project_doc("custom/skills/board-x/SKILL.md", "ok\n");
    env.write_project_doc("custom/skills/project-y/SKILL.md", "bad\n");
    // A directory under the default `.agents/skills` must be ignored because the
    // policy points the scan at `custom/skills`.
    env.write_project_doc(".agents/skills/helper-z/SKILL.md", "ignored\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    let json = out.json();
    assert_eq!(
        skill_results(&json),
        vec![
            ("board-x".to_string(), true),
            ("project-y".to_string(), false),
        ],
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn audit_rejects_prefix_only_name() {
    // `project-` with nothing after the hyphen fails, mirroring the
    // create-project-skill rule `^(project|private)-[a-z0-9-]+`.
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/project-/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    assert_eq!(
        skill_results(&out.json()),
        vec![("project-".to_string(), false)]
    );
}

#[test]
fn audit_rejects_uppercase_skill_name() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/project-Bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project", "--format", "json"]);
    assert_eq!(
        skill_results(&out.json()),
        vec![("project-Bad".to_string(), false)]
    );
}

#[test]
fn audit_text_output_includes_skills_section() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\n");
    env.write_project_doc(".agents/skills/helper-bad/SKILL.md", "bad\n");

    let out = env.run(&["audit", "--target", "project"]);
    assert!(out.stdout.contains("skills:"), "stdout: {}", out.stdout);
    assert!(
        out.stdout.contains("[FAIL] helper-bad"),
        "stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("rename skill 'helper-bad'"),
        "stdout: {}",
        out.stdout
    );
}

#[test]
fn audit_rejects_unknown_skills_field() {
    let env = TestEnv::new();
    env.write_project_catalog("[skills]\nenforce_name_prefix = true\nbogus = 1\n");
    let out = env.run(&["audit", "--target", "project"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("skills.bogus") && out.stderr.contains("unsupported field"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn audit_rejects_non_table_skills() {
    let env = TestEnv::new();
    env.write_project_catalog("skills = 5\n");
    let out = env.run(&["audit", "--target", "project"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("must be a [skills] table"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn audit_rejects_bad_prefix_value() {
    let env = TestEnv::new();
    env.write_project_catalog(
        "[skills]\nenforce_name_prefix = true\nallowed_prefixes = [\"Bad_Prefix\"]\n",
    );
    let out = env.run(&["audit", "--target", "project"]);
    assert_eq!(out.code, EXIT_CONFIG, "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("skills.allowed_prefixes") && out.stderr.contains("kebab-case"),
        "stderr: {}",
        out.stderr
    );
}
