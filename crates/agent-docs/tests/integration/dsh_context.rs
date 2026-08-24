use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;

use super::common::TestEnv;

fn context_args<'a>(state: &'a str, request_id: &'a str) -> Vec<&'a str> {
    vec![
        "session",
        "context",
        "--session-id",
        "dsh-session-private-sentinel",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--request-id",
        request_id,
        "--format",
        "json",
    ]
}

fn prerequisite_args<'a>(state: &'a str, request_id: &'a str) -> Vec<&'a str> {
    vec![
        "session",
        "prerequisite",
        "--session-id",
        "dsh-session-private-sentinel",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--request-id",
        request_id,
        "--agent-id",
        "dsh-agent-private-sentinel",
        "--workspace-generation",
        "workspace-generation-private-sentinel",
        "--call-id",
        "dsh-call-private-sentinel",
        "--turn",
        "1",
        "--step",
        "2",
        "--tool-name",
        "write",
        "--definition-id",
        "definition-private-sentinel",
        "--format",
        "json",
    ]
}

fn commit_prerequisite_args<'a>(state: &'a str, receipt: &'a str) -> Vec<&'a str> {
    vec![
        "session",
        "commit-prerequisite",
        "--session-id",
        "dsh-session-private-sentinel",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--receipt",
        receipt,
        "--agent-id",
        "dsh-agent-private-sentinel",
        "--workspace-generation",
        "workspace-generation-private-sentinel",
        "--call-id",
        "dsh-call-private-sentinel",
        "--turn",
        "1",
        "--step",
        "2",
        "--tool-name",
        "write",
        "--definition-id",
        "definition-private-sentinel",
        "--format",
        "json",
    ]
}

fn replace_arg(args: Vec<&str>, flag: &str, value: impl Into<String>) -> Vec<String> {
    let mut args: Vec<String> = args.into_iter().map(str::to_string).collect();
    let index = args
        .iter()
        .position(|argument| argument == flag)
        .unwrap_or_else(|| panic!("missing test argument {flag}"));
    args[index + 1] = value.into();
    args
}

fn string_args(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn session_records(state_home: &Path) -> Vec<PathBuf> {
    let sessions = state_home.join("agent-docs/sessions");
    if !sessions.exists() {
        return Vec::new();
    }
    let mut pending = vec![sessions];
    let mut records = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("session state directory") {
            let path = entry.expect("session state entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                records.push(path);
            }
        }
    }
    records.sort();
    records
}

fn only_session_record(state_home: &Path) -> PathBuf {
    let records = session_records(state_home);
    assert_eq!(records.len(), 1, "expected one DSH session record");
    records.into_iter().next().unwrap()
}

#[test]
fn dsh_prerequisite_begin_is_side_effect_free_and_commit_is_exact_and_idempotent() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let begun = env.run(&prerequisite_args(state, "prerequisite-request-1"));
    assert_eq!(begun.code, 0, "stderr={}", begun.stderr);
    let json = begun.json();
    assert_eq!(
        json["schema_version"],
        "cli.agent-docs.session.prerequisite.v1"
    );
    let decision = &json["data"]["decision"];
    assert_eq!(decision["schema_version"], "decision.prerequisite.v1");
    assert_eq!(decision["request_id"], "prerequisite-request-1");
    assert_eq!(decision["product"], "dsh");
    assert_eq!(decision["intent"], "project-dev");
    assert_eq!(decision["phase"], "edit");
    assert_eq!(decision["reason"], "pending");
    assert_eq!(decision["verified"], true);
    assert_eq!(decision["documents"][0]["content"], "bounded policy\n");
    let receipt = decision["receipt"]
        .as_str()
        .expect("pending prerequisite receipt");
    assert!(receipt.len() <= 4096);
    assert!(session_records(&state_home).is_empty());
    for private in [
        "dsh-session-private-sentinel",
        "dsh-agent-private-sentinel",
        "workspace-generation-private-sentinel",
        "dsh-call-private-sentinel",
        "definition-private-sentinel",
        env.project.to_str().unwrap(),
    ] {
        assert!(!begun.stdout.contains(private), "leaked {private:?}");
    }

    let committed = env.run(&commit_prerequisite_args(state, receipt));
    assert_eq!(committed.code, 0, "stderr={}", committed.stderr);
    assert_eq!(
        committed.json()["schema_version"],
        "cli.agent-docs.session.commit-prerequisite.v1"
    );
    assert_eq!(committed.json()["data"]["reason"], "prepared");
    assert_eq!(session_records(&state_home).len(), 1);

    let replayed = env.run(&commit_prerequisite_args(state, receipt));
    assert_eq!(replayed.code, 0, "stderr={}", replayed.stderr);
    assert_eq!(replayed.json()["data"]["reason"], "already-current");

    let reused = env.run(&prerequisite_args(state, "prerequisite-request-2"));
    assert_eq!(reused.code, 0, "stderr={}", reused.stderr);
    assert_eq!(
        reused.json()["data"]["decision"]["reason"],
        "already-current"
    );
    assert!(reused.json()["data"]["decision"]["receipt"].is_string());
}

#[test]
fn reused_dsh_prerequisite_is_revalidated_at_the_execution_boundary() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let first = env.run(&prerequisite_args(state, "reuse-first"));
    let first_json = first.json();
    let first_receipt = first_json["data"]["decision"]["receipt"]
        .as_str()
        .expect("first prerequisite receipt");
    let committed = env.run(&commit_prerequisite_args(state, first_receipt));
    assert_eq!(committed.code, 0, "stderr={}", committed.stderr);

    let reused = env.run(&prerequisite_args(state, "reuse-second"));
    assert_eq!(
        reused.json()["data"]["decision"]["reason"],
        "already-current"
    );
    let reused_json = reused.json();
    let reused_receipt = reused_json["data"]["decision"]["receipt"]
        .as_str()
        .expect("reuse must retain an execution-bound receipt");

    env.write_project_doc("POLICY.md", "changed during approval\n");
    let stale = env.run(&commit_prerequisite_args(state, reused_receipt));
    assert_ne!(stale.code, 0, "stdout={}", stale.stdout);
    assert_eq!(stale.json()["error"]["code"], "prerequisite-stale");
}

#[test]
fn concurrent_dsh_prerequisite_commits_converge_without_corrupting_session_state() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let first_begin = env.run(&prerequisite_args(state, "concurrent-first"));
    let second_begin_args = replace_arg(
        prerequisite_args(state, "concurrent-second"),
        "--call-id",
        "dsh-call-concurrent-second",
    );
    let second_begin = env.run(&string_args(&second_begin_args));
    assert_eq!(first_begin.code, 0, "stderr={}", first_begin.stderr);
    assert_eq!(second_begin.code, 0, "stderr={}", second_begin.stderr);
    let first_json = first_begin.json();
    let second_json = second_begin.json();
    let first_receipt = first_json["data"]["decision"]["receipt"].as_str().unwrap();
    let second_receipt = second_json["data"]["decision"]["receipt"].as_str().unwrap();

    let first_commit = commit_prerequisite_args(state, first_receipt);
    let second_commit = replace_arg(
        commit_prerequisite_args(state, second_receipt),
        "--call-id",
        "dsh-call-concurrent-second",
    );
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| env.run(&first_commit));
        let second = scope.spawn(|| env.run(&string_args(&second_commit)));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    assert_eq!(second.code, 0, "stderr={}", second.stderr);
    let mut reasons = [
        first.json()["data"]["reason"].as_str().unwrap().to_string(),
        second.json()["data"]["reason"]
            .as_str()
            .unwrap()
            .to_string(),
    ];
    reasons.sort();
    assert_eq!(reasons, ["already-current", "prepared"]);
    assert_eq!(session_records(&state_home).len(), 1);

    let reused = env.run(&prerequisite_args(state, "concurrent-reuse"));
    assert_eq!(reused.code, 0, "stderr={}", reused.stderr);
    assert_eq!(
        reused.json()["data"]["decision"]["reason"],
        "already-current"
    );
}

#[test]
fn dsh_prerequisite_unsatisfied_policy_is_typed_side_effect_free_and_content_safe() {
    for content in [None, Some("")] {
        let env = TestEnv::new();
        env.write_project_catalog(
            r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
        );
        if let Some(content) = content {
            env.write_project_doc("POLICY.md", content);
        }
        let state_home = env.project_path("state");
        let failed = env.run(&prerequisite_args(
            state_home.to_str().unwrap(),
            "unsatisfied-prerequisite",
        ));
        assert_eq!(failed.code, 65, "stdout={}", failed.stdout);
        let failure = failed.json();
        assert_eq!(
            failure["schema_version"],
            "cli.agent-docs.session.prerequisite.v1"
        );
        assert_eq!(failure["error"]["code"], "phase-unsatisfied");
        assert_eq!(failure["error"]["details"]["next_action"], "repair-catalog");
        assert!(!failed.stdout.contains("\"content\""));
        assert!(session_records(&state_home).is_empty());
    }
}

#[test]
fn dsh_phase_prerequisite_materializes_a_phase_activation_from_a_full_activation() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let full = env.run(&context_args(state, "full-context"));
    assert_eq!(full.code, 0, "stderr={}", full.stderr);

    let begun = env.run(&prerequisite_args(state, "phase-prerequisite"));
    assert_eq!(begun.code, 0, "stderr={}", begun.stderr);
    assert_eq!(begun.json()["data"]["decision"]["reason"], "pending");
    let begun_json = begun.json();
    let receipt = begun_json["data"]["decision"]["receipt"]
        .as_str()
        .expect("phase prerequisite receipt");

    let committed = env.run(&commit_prerequisite_args(state, receipt));
    assert_eq!(committed.code, 0, "stderr={}", committed.stderr);
    assert_eq!(committed.json()["data"]["reason"], "prepared");

    let reused = env.run(&prerequisite_args(state, "phase-prerequisite-reuse"));
    assert_eq!(reused.code, 0, "stderr={}", reused.stderr);
    assert_eq!(
        reused.json()["data"]["decision"]["reason"],
        "already-current"
    );
}

#[test]
fn dsh_prerequisite_requires_edit_phase_and_the_verifier_catalog_selection() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let args = prerequisite_args(state, "missing-phase");
    let without_phase: Vec<&str> = args
        .into_iter()
        .filter(|value| *value != "--phase" && *value != "edit")
        .collect();
    let missing = env.run(&without_phase);
    assert_eq!(missing.code, 64, "stdout={}", missing.stdout);

    let mut unsupported = vec!["--user-config"];
    unsupported.extend(prerequisite_args(state, "unsupported-selection"));
    let unsupported = env.run(&unsupported);
    assert_eq!(unsupported.code, 65, "stdout={}", unsupported.stdout);
    assert_eq!(
        unsupported.json()["error"]["code"],
        "unsupported-prerequisite-selection"
    );

    let mut local_only = vec!["--worktree-fallback", "local-only"];
    local_only.extend(prerequisite_args(state, "unsupported-fallback"));
    let local_only = env.run(&local_only);
    assert_eq!(local_only.code, 65, "stdout={}", local_only.stdout);
    assert_eq!(
        local_only.json()["error"]["code"],
        "unsupported-prerequisite-selection"
    );
}

#[test]
fn dsh_prerequisite_commit_rejects_stale_and_cross_scope_receipts_without_activation() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
phase = "edit"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "policy before change\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let begun = env.run(&prerequisite_args(state, "prerequisite-stale"));
    assert_eq!(begun.code, 0, "stderr={}", begun.stderr);
    let begun_json = begun.json();
    let receipt = begun_json["data"]["decision"]["receipt"].as_str().unwrap();
    env.write_project_doc("POLICY.md", "policy after change\n");

    let stale = env.run(&commit_prerequisite_args(state, receipt));
    assert_eq!(stale.code, 65, "stdout={}", stale.stdout);
    assert_eq!(stale.json()["error"]["code"], "prerequisite-stale");
    assert_eq!(
        stale.json()["error"]["details"]["recovery"]["intents"],
        serde_json::json!(["project-dev"])
    );
    assert_eq!(
        stale.json()["error"]["details"]["recovery"]["phase"],
        "edit"
    );
    assert!(session_records(&state_home).is_empty());

    let fresh = env.run(&prerequisite_args(state, "prerequisite-cross-scope"));
    assert_eq!(fresh.code, 0, "stderr={}", fresh.stderr);
    let fresh_json = fresh.json();
    let fresh_receipt = fresh_json["data"]["decision"]["receipt"].as_str().unwrap();
    let other_state = env.project_path("other-state");
    for (flag, value) in [
        ("--session-id", "other-session".to_string()),
        ("--state-home", other_state.to_string_lossy().into_owned()),
        ("--agent-id", "other-agent".to_string()),
        (
            "--workspace-generation",
            "other-workspace-generation".to_string(),
        ),
        ("--call-id", "other-call".to_string()),
        ("--turn", "2".to_string()),
        ("--step", "3".to_string()),
        ("--tool-name", "edit".to_string()),
        ("--definition-id", "other-definition".to_string()),
    ] {
        let foreign_args = replace_arg(commit_prerequisite_args(state, fresh_receipt), flag, value);
        let foreign = env.run(&string_args(&foreign_args));
        assert_eq!(foreign.code, 65, "flag={flag} stdout={}", foreign.stdout);
        assert_eq!(
            foreign.json()["error"]["code"],
            "prerequisite-receipt-mismatch",
            "flag={flag}"
        );
    }

    let other_project = env.project_path("other-project");
    std::fs::create_dir_all(&other_project).expect("other project");
    let foreign_project = env.run_for_project(
        &other_project,
        &commit_prerequisite_args(state, fresh_receipt),
    );
    assert_eq!(
        foreign_project.code, 65,
        "stdout={}",
        foreign_project.stdout
    );
    assert_eq!(
        foreign_project.json()["error"]["code"],
        "prerequisite-receipt-mismatch"
    );
    assert!(session_records(&state_home).is_empty());
}

#[test]
fn dsh_prerequisite_text_errors_name_the_invoked_command() {
    let env = TestEnv::new();
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let mut args = commit_prerequisite_args(state, "malformed");
    args.truncate(args.len() - 2);

    let failed = env.run(&args);
    assert_eq!(failed.code, 65, "stdout={}", failed.stdout);
    assert!(
        failed
            .stderr
            .contains("agent-docs session commit-prerequisite:"),
        "stderr={}",
        failed.stderr
    );
}

#[test]
fn dsh_context_returns_only_satisfied_required_document_content() {
    let env = TestEnv::new();
    env.write_home_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "home"
path = "HOME_POLICY.md"
product = "dsh"
required = true
"#,
    )
    .write_home_doc("HOME_POLICY.md", "home policy\n")
    .write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "PROJECT_POLICY.md"
product = "dsh"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "OPTIONAL_PRIVATE_SENTINEL.md"
product = "dsh"
required = false

[[document]]
context = "project-dev"
scope = "project"
path = "CODEX_ONLY_SENTINEL.md"
product = "codex"
required = true

[[validation]]
context = "project-dev"
product = "dsh"
commands = ["VALIDATION_COMMAND_PRIVATE_SENTINEL"]
"#,
    )
    .write_project_doc("PROJECT_POLICY.md", "project policy\n")
    .write_project_doc(
        "OPTIONAL_PRIVATE_SENTINEL.md",
        "optional content private sentinel\n",
    )
    .write_project_doc("CODEX_ONLY_SENTINEL.md", "codex-only private sentinel\n");
    let state_home = env.project_path("ABSOLUTE_STATE_HOME_SENTINEL");
    let state = state_home.to_str().unwrap();

    let first = env.run(&context_args(state, "request-0001"));
    assert_eq!(first.code, 0, "stderr={}", first.stderr);
    let json = first.json();
    assert_eq!(json["schema_version"], "cli.agent-docs.session.context.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(
        json.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["data", "ok", "schema_version"])
    );
    assert_eq!(
        json["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["decision"])
    );
    let decision = &json["data"]["decision"];
    assert_eq!(decision["schema_version"], "decision.context.v1");
    assert_eq!(decision["request_id"], "request-0001");
    assert_eq!(decision["product"], "dsh");
    assert_eq!(decision["intent"], "project-dev");
    assert_eq!(decision["reason"], "prepared");
    assert_eq!(decision["verified"], true);
    assert_eq!(decision["document_count"], 2);
    assert_eq!(decision["total_bytes"], 27);
    assert_eq!(
        decision
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "document_count",
            "documents",
            "intent",
            "product",
            "reason",
            "request_id",
            "schema_version",
            "total_bytes",
            "verified",
        ])
    );
    assert_eq!(
        decision["documents"],
        serde_json::json!([
            {"source": "home", "scope": "home", "content": "home policy\n"},
            {"source": "project", "scope": "project", "content": "project policy\n"}
        ])
    );
    for private in [
        "dsh-session-private-sentinel",
        "ABSOLUTE_STATE_HOME_SENTINEL",
        env.project.to_str().unwrap(),
        "HOME_POLICY.md",
        "PROJECT_POLICY.md",
        "OPTIONAL_PRIVATE_SENTINEL",
        "optional content private sentinel",
        "CODEX_ONLY_SENTINEL",
        "codex-only private sentinel",
        "VALIDATION_COMMAND_PRIVATE_SENTINEL",
    ] {
        assert!(!first.stdout.contains(private), "leaked {private:?}");
    }

    let second = env.run(&context_args(state, "request-0002"));
    assert_eq!(second.code, 0, "stderr={}", second.stderr);
    let second_json = second.json();
    let second_decision = &second_json["data"]["decision"];
    assert_eq!(second_decision["reason"], "already-current");
    assert_eq!(second_decision["request_id"], "request-0002");
    for field in [
        "schema_version",
        "product",
        "intent",
        "verified",
        "documents",
        "document_count",
        "total_bytes",
    ] {
        assert_eq!(
            second_decision[field], decision[field],
            "already-current changed policy field {field}"
        );
    }
}

#[test]
fn dsh_context_budget_failure_does_not_activate_or_emit_partial_policy() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "POLICY_CONTENT_MUST_NOT_BE_PARTIAL\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let mut args = context_args(state, "request-budget");
    args.splice(args.len() - 2..args.len() - 2, ["--max-bytes", "4"]);

    let output = env.run(&args);
    assert_eq!(output.code, 65, "stderr={}", output.stderr);
    assert_eq!(
        output.json()["schema_version"],
        "cli.agent-docs.session.context.v1"
    );
    assert_eq!(output.json()["error"]["code"], "context-budget-exceeded");
    assert!(!output.stdout.contains("POLICY_CONTENT_MUST_NOT_BE_PARTIAL"));

    assert!(session_records(&state_home).is_empty());

    let prepared = env.run(&context_args(state, "request-prepared"));
    assert_eq!(prepared.code, 0, "stderr={}", prepared.stderr);
    let record_path = only_session_record(&state_home);
    let before = fs::read(&record_path).unwrap();

    let mut failed_refresh = context_args(state, "request-budget-existing");
    failed_refresh.splice(
        failed_refresh.len() - 2..failed_refresh.len() - 2,
        ["--max-bytes", "4"],
    );
    let failed_refresh = env.run(&failed_refresh);
    assert_eq!(failed_refresh.code, 65, "stderr={}", failed_refresh.stderr);
    assert_eq!(
        failed_refresh.json()["error"]["code"],
        "context-budget-exceeded"
    );
    assert_eq!(fs::read(&record_path).unwrap(), before);

    let preserved = env.run(&context_args(state, "request-preserved"));
    assert_eq!(preserved.code, 0, "stderr={}", preserved.stderr);
    assert_eq!(
        preserved.json()["data"]["decision"]["reason"],
        "already-current"
    );
}

#[test]
fn dsh_context_validates_request_id_budget_and_exactly_one_intent() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    for request_id in ["", "unsafe request", "../unsafe", &"x".repeat(129)] {
        let output = env.run(&context_args(state, request_id));
        assert_eq!(output.code, 65, "request_id={request_id:?}");
        assert_eq!(output.json()["error"]["code"], "invalid-request-id");
    }

    let mut too_large = context_args(state, "request-hard-cap");
    too_large.splice(
        too_large.len() - 2..too_large.len() - 2,
        ["--max-bytes", "65537"],
    );
    let output = env.run(&too_large);
    assert_eq!(output.code, 65);
    assert_eq!(output.json()["error"]["code"], "invalid-max-bytes");

    let duplicated = env.run(&[
        "session",
        "context",
        "--session-id",
        "session",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--intent",
        "task-tools",
        "--request-id",
        "request-duplicate-intent",
        "--format",
        "json",
    ]);
    assert_eq!(duplicated.code, 64, "stdout={}", duplicated.stdout);
    assert_eq!(duplicated.json()["error"]["code"], "parse-error");

    let mut wrong_product = context_args(state, "request-wrong-product");
    let product = wrong_product
        .iter_mut()
        .find(|value| **value == "dsh")
        .unwrap();
    *product = "codex";
    let wrong_product = env.run(&wrong_product);
    assert_eq!(wrong_product.code, 64, "stdout={}", wrong_product.stdout);
    assert_eq!(wrong_product.json()["error"]["code"], "parse-error");
}

#[test]
fn dsh_context_phase_and_session_scope_remain_isolated() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "COMMON.md"
product = "dsh"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "EDIT.md"
product = "dsh"
phase = "edit"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "DELIVERY.md"
product = "dsh"
phase = "delivery"
required = true

[[document]]
context = "task-tools"
scope = "project"
path = "TOOLS.md"
product = "dsh"
required = true
"#,
    )
    .write_project_doc("COMMON.md", "common\n")
    .write_project_doc("EDIT.md", "edit\n")
    .write_project_doc("TOOLS.md", "tools\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let mut args = context_args(state, "request-phase");
    args.splice(args.len() - 2..args.len() - 2, ["--phase", "edit"]);
    let output = env.run(&args);
    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    let decision = &output.json()["data"]["decision"];
    assert_eq!(decision["phase"], "edit");
    assert_eq!(decision["document_count"], 2);
    assert!(!output.stdout.contains("DELIVERY.md"));

    let mut same_phase_args = context_args(state, "request-same-phase");
    same_phase_args.splice(
        same_phase_args.len() - 2..same_phase_args.len() - 2,
        ["--phase", "edit"],
    );
    let same_phase = env.run(&same_phase_args);
    assert_eq!(same_phase.code, 0, "stderr={}", same_phase.stderr);
    assert_eq!(
        same_phase.json()["data"]["decision"]["reason"],
        "already-current"
    );

    let mut other_phase_args = context_args(state, "request-other-phase");
    other_phase_args.splice(
        other_phase_args.len() - 2..other_phase_args.len() - 2,
        ["--phase", "delivery"],
    );
    let other_phase = env.run(&other_phase_args);
    assert_eq!(other_phase.code, 65, "stdout={}", other_phase.stdout);
    assert_eq!(other_phase.json()["error"]["code"], "phase-unsatisfied");
    assert_eq!(
        other_phase.json()["error"]["details"]["next_action"],
        "repair-catalog"
    );
    assert_eq!(
        other_phase.json()["error"]["details"]["recovery"]["action"],
        "repair-catalog"
    );
    assert!(
        other_phase.json()["error"]["details"]["recovery"]
            .get("command")
            .is_none()
    );

    let other_session = env.run(&[
        "session",
        "context",
        "--session-id",
        "other-session",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--phase",
        "edit",
        "--request-id",
        "request-other-session",
        "--format",
        "json",
    ]);
    assert_eq!(other_session.code, 0, "stdout={}", other_session.stdout);
    assert_eq!(
        other_session.json()["data"]["decision"]["reason"],
        "prepared"
    );
}

#[test]
fn dsh_context_default_budget_is_full_without_phase_and_selective_with_phase() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "COMMON.md"
product = "dsh"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "EDIT.md"
product = "dsh"
phase = "edit"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "DELIVERY.md"
product = "dsh"
phase = "delivery"
required = true
"#,
    )
    .write_project_doc("COMMON.md", &"c".repeat(8 * 1024))
    .write_project_doc("EDIT.md", &"e".repeat(8 * 1024))
    .write_project_doc("DELIVERY.md", &"d".repeat(8 * 1024));
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let full = env.run(&context_args(state, "request-full-budget"));
    assert_eq!(full.code, 65, "stdout={}", full.stdout);
    assert_eq!(full.json()["error"]["code"], "context-budget-exceeded");

    let mut edit_args = context_args(state, "request-edit-budget");
    edit_args.splice(
        edit_args.len() - 2..edit_args.len() - 2,
        ["--phase", "edit"],
    );
    let edit = env.run(&edit_args);
    assert_eq!(edit.code, 0, "stderr={}", edit.stderr);
    let decision = &edit.json()["data"]["decision"];
    assert_eq!(decision["phase"], "edit");
    assert_eq!(decision["document_count"], 2);
    assert_eq!(decision["total_bytes"], 16 * 1024);
    assert_eq!(decision["reason"], "prepared");
}

#[test]
fn dsh_context_rejects_catalog_paths_outside_their_declared_scope() {
    for path_kind in ["absolute", "parent"] {
        let env = TestEnv::new();
        let secret = env.home_path("OUTSIDE_SCOPE_SECRET.md");
        fs::write(&secret, "OUTSIDE_SCOPE_SECRET_SENTINEL\n").unwrap();
        let declared_path = match path_kind {
            "absolute" => secret.display().to_string(),
            "parent" => "../docs-home/OUTSIDE_SCOPE_SECRET.md".to_string(),
            _ => unreachable!(),
        };
        env.write_project_catalog(&format!(
            r#"
[[document]]
context = "project-dev"
scope = "project"
path = {declared_path:?}
product = "dsh"
required = true
"#
        ));
        let state_home = env.project_path("state");
        let output = env.run(&context_args(
            state_home.to_str().unwrap(),
            &format!("request-outside-{path_kind}"),
        ));
        assert_eq!(output.code, 65, "stdout={}", output.stdout);
        assert_eq!(output.json()["error"]["code"], "context-document-unsafe");
        assert!(!output.stdout.contains("OUTSIDE_SCOPE_SECRET_SENTINEL"));
    }
}

#[cfg(unix)]
#[test]
fn dsh_context_rejects_a_required_document_symlink() {
    use std::os::unix::fs::symlink;

    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true
"#,
    );
    let secret = env.home_path("SYMLINK_SECRET.md");
    fs::write(&secret, "SYMLINK_SECRET_SENTINEL\n").unwrap();
    symlink(secret, env.project_path("POLICY.md")).unwrap();
    let state_home = env.project_path("state");
    let output = env.run(&context_args(
        state_home.to_str().unwrap(),
        "request-symlink",
    ));
    assert_eq!(output.code, 65, "stdout={}", output.stdout);
    assert_eq!(output.json()["error"]["code"], "context-document-unsafe");
    assert!(!output.stdout.contains("SYMLINK_SECRET_SENTINEL"));
}

#[test]
fn dsh_context_skips_optional_content_before_reading_and_enforces_required_limits() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "OPTIONAL_HUGE.md"
product = "dsh"
required = false
"#,
    )
    .write_project_doc("POLICY.md", "bounded policy\n")
    .write_project_doc("OPTIONAL_HUGE.md", &"o".repeat(1024 * 1024));
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let output = env.run(&context_args(state, "request-optional-huge"));
    assert_eq!(output.code, 0, "stdout={}", output.stdout);
    assert_eq!(output.json()["data"]["decision"]["document_count"], 1);
    assert_eq!(output.json()["data"]["decision"]["total_bytes"], 15);

    env.write_project_doc("POLICY.md", &"r".repeat(64 * 1024));
    let mut at_limit = context_args(state, "request-required-at-limit");
    at_limit.splice(
        at_limit.len() - 2..at_limit.len() - 2,
        ["--max-bytes", "65536"],
    );
    let at_limit = env.run(&at_limit);
    assert_eq!(at_limit.code, 0, "stdout={}", at_limit.stdout);
    assert_eq!(at_limit.json()["data"]["decision"]["total_bytes"], 65536);

    env.write_project_doc("POLICY.md", &"r".repeat(64 * 1024 + 1));
    let mut over_limit = context_args(state, "request-required-over-limit");
    over_limit.splice(
        over_limit.len() - 2..over_limit.len() - 2,
        ["--max-bytes", "65536"],
    );
    let required = env.run(&over_limit);
    assert_eq!(required.code, 65, "stdout={}", required.stdout);
    assert_eq!(required.json()["error"]["code"], "context-budget-exceeded");
}

#[test]
fn dsh_context_caps_the_number_of_required_documents() {
    let env = TestEnv::new();
    let mut catalog = String::new();
    for index in 0..128 {
        catalog.push_str(&format!(
            r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY_{index}.md"
product = "dsh"
required = true
"#
        ));
        env.write_project_doc(&format!("POLICY_{index}.md"), "x");
    }
    env.write_project_catalog(&catalog);
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let at_limit = env.run(&context_args(state, "request-document-count-limit"));
    assert_eq!(at_limit.code, 0, "stdout={}", at_limit.stdout);
    assert_eq!(at_limit.json()["data"]["decision"]["document_count"], 128);
    assert_eq!(at_limit.json()["data"]["decision"]["total_bytes"], 128);

    catalog.push_str(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY_128.md"
product = "dsh"
required = true
"#,
    );
    env.write_project_doc("POLICY_128.md", "x");
    env.write_project_catalog(&catalog);
    let output = env.run(&context_args(state, "request-document-count"));
    assert_eq!(output.code, 65, "stdout={}", output.stdout);
    assert_eq!(output.json()["error"]["code"], "context-budget-exceeded");
}

#[test]
fn dsh_context_fails_closed_when_policy_exceeds_the_hard_limit() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "small policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();

    let prepared = env.run(&context_args(state, "request-before-growth"));
    assert_eq!(prepared.code, 0, "stderr={}", prepared.stderr);
    let record_path = only_session_record(&state_home);
    let before = fs::read(&record_path).expect("prepared record");

    env.write_project_doc("POLICY.md", &"x".repeat(64 * 1024 + 1));
    let mut args = context_args(state, "request-after-growth");
    args.splice(args.len() - 2..args.len() - 2, ["--max-bytes", "65536"]);
    let output = env.run(&args);
    assert_eq!(output.code, 65, "stdout={}", output.stdout);
    assert_eq!(output.json()["error"]["code"], "context-budget-exceeded");
    assert_eq!(fs::read(record_path).expect("record after failure"), before);
}

#[test]
fn generic_session_surfaces_do_not_expand_the_stable_product_contract() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "POLICY.md"
product = "dsh"
required = true
"#,
    )
    .write_project_doc("POLICY.md", "policy\n");
    let state_home = env.project_path("state");
    let state = state_home.to_str().unwrap();
    let prepare = env.run(&[
        "session",
        "prepare",
        "--session-id",
        "standard-dsh-session",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(prepare.code, 64, "stderr={}", prepare.stderr);
    assert_eq!(prepare.json()["error"]["code"], "parse-error");

    let verify = env.run(&[
        "session",
        "verify",
        "--session-id",
        "standard-dsh-session",
        "--product",
        "dsh",
        "--state-home",
        state,
        "--require-intent",
        "project-dev",
        "--format",
        "json",
    ]);
    assert_eq!(verify.code, 64, "stderr={}", verify.stderr);
    assert_eq!(verify.json()["error"]["code"], "parse-error");
}

#[test]
fn standard_catalog_filter_and_dsh_integration_resolve_are_isolated() {
    let env = TestEnv::new();
    env.write_project_catalog(
        r#"
[[document]]
context = "project-dev"
scope = "project"
path = "DSH.md"
product = "dsh"
required = true

[[document]]
context = "project-dev"
scope = "project"
path = "CODEX.md"
product = "codex"
required = true
"#,
    )
    .write_project_doc("DSH.md", "dsh policy\n")
    .write_project_doc("CODEX.md", "codex policy\n");

    let list = env.run(&["list", "--product", "codex", "--format", "json"]);
    assert_eq!(list.code, 0, "stderr={}", list.stderr);
    assert_eq!(list.json()["documents"].as_array().unwrap().len(), 1);
    assert_eq!(list.json()["documents"][0]["products"][0], "codex");

    let integration = env.run(&[
        "integration",
        "resolve",
        "--product",
        "dsh",
        "--format",
        "json",
    ]);
    assert_eq!(integration.code, 0, "stderr={}", integration.stderr);
    assert_eq!(integration.json()["data"]["product"], "dsh");
    assert_eq!(integration.json()["data"]["action"], "integrate");
}
