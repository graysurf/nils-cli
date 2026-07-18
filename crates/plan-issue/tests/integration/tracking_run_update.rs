//! `plan-issue tracking run init` + `tracking run update` integration
//! coverage (Task 5.1).
//!
//! Source: `docs/source/plan-issue-redesign/plan-tracking-issue-run-state-controller-v1.md`.

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::Value;
use tempfile::TempDir;

use plan_issue::tracking::run_state::{self, RUN_STATE_SCHEMA};

use crate::common;

#[test]
fn tracking_run_update_init_writes_run_state_and_events() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "1.2",
        "--branch",
        "feat/x",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-test",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    assert_eq!(envelope["command"], "tracking.run.init");
    let result = &envelope["payload"]["result"];
    assert_eq!(result["run_id"], "run-test");
    assert!(run_state_path.exists());
    let events_path = run_state_path
        .parent()
        .map(|p| p.join("events.jsonl"))
        .expect("events");
    assert!(events_path.exists(), "events.jsonl should exist");

    // Verify schema id stayed stable.
    let raw = fs::read_to_string(&run_state_path).expect("read");
    assert!(raw.contains(RUN_STATE_SCHEMA));
}

#[test]
fn tracking_run_init_rejects_noncanonical_linked_pr_before_persistence() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--linked-pr",
        "prefix Authorization: Bearer abc~def+/== suffix",
        "--now",
        "2026-07-18T00:00:00Z",
        "--run-id",
        "invalid-linked-pr",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);

    assert_eq!(out.code, 64, "stderr: {}", out.stderr_text());
    let rendered = format!("{}\n{}", out.stdout_text(), out.stderr_text());
    assert!(rendered.contains("record-invalid-linked-pr"), "{rendered}");
    assert!(!rendered.contains("abc~def+/=="), "{rendered}");
    assert!(!run_state_path.exists());
    assert!(!tmp.path().join("events.jsonl").exists());
}

#[test]
fn tracking_run_init_refuses_busy_output_lock_without_replacing_state() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let lock_path = tmp.path().join(".run-state.json.update.lock");
    fs::write(&run_state_path, "existing generation\n").expect("existing state");
    let _active_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&lock_path)
        .expect("hold advisory lock");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-07-18T00:00:00Z",
        "--run-id",
        "replacement-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-run-update-lock-busy"
    );
    assert_eq!(
        fs::read_to_string(&run_state_path).expect("unchanged state"),
        "existing generation\n"
    );
    assert!(lock_path.exists(), "stable advisory lock path missing");
    assert!(!tmp.path().join("events.jsonl").exists());
}

#[test]
fn tracking_run_init_redacts_repository_credentials_from_output_and_events() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let provider_repo = "https://operator:secret-token@gitlab.example.test/acme/widgets.git";

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            provider_repo,
            "--issue",
            "1271",
            "--now",
            "2026-07-18T00:00:00Z",
            "--run-id",
            "credential-redaction-run",
            "--out",
            run_state_path.to_str().expect("run-state path"),
        ],
        common::plan_issue_cmd_options().with_cwd(tmp.path()),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["repo"], "acme/widgets");
    assert_eq!(result["repo_provider"], "gitlab");
    assert_eq!(result["repo_host"], "gitlab.example.test");
    assert!(!out.stdout_text().contains("operator"));
    assert!(!out.stdout_text().contains("secret-token"));

    let events = fs::read_to_string(tmp.path().join("events.jsonl")).expect("events");
    assert!(events.contains("\"repo\":\"acme/widgets\""), "{events}");
    assert!(
        events.contains("\"repo_host\":\"gitlab.example.test\""),
        "{events}"
    );
    assert!(!events.contains("operator"), "{events}");
    assert!(!events.contains("secret-token"), "{events}");
}

#[test]
fn tracking_run_init_records_repository_relative_state_identity() {
    let checkout_dir = TempDir::new().expect("checkout fixture");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(checkout_dir.path())
            .status()
            .expect("run git")
    };
    assert!(git(&["init", "-q"]).success());
    assert!(git(&["remote", "add", "origin", "git@gitlab.com:acme/widgets.git",]).success());

    let repo_root = checkout_dir.path();
    let bundle = repo_root.join("bundle");
    fs::create_dir(&bundle).expect("bundle");
    let execution_state = bundle.join("portable-execution-state.md");
    fs::write(&execution_state, "## Task Ledger\n").expect("execution state");
    let run_state_path = repo_root.join("run-state.json");
    let provider_repo = "https://gitlab.com/acme/widgets.git";

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            provider_repo,
            "--issue",
            "1271",
            "--bundle",
            bundle.to_str().expect("bundle path"),
            "--execution-state-file",
            execution_state.to_str().expect("execution-state path"),
            "--now",
            "2026-07-17T00:00:00Z",
            "--run-id",
            "portable-run",
            "--out",
            run_state_path.to_str().expect("run-state path"),
        ],
        common::plan_issue_cmd_options().with_cwd(repo_root),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let raw = fs::read_to_string(&run_state_path).expect("raw run state");
    let value: Value = serde_json::from_str(&raw).expect("run-state json");
    assert_eq!(value["repo"], "acme/widgets");
    assert_eq!(value["repo_provider"], "gitlab");
    assert_eq!(value["repo_host"], "gitlab.com");

    let run = run_state::read_run_state(&run_state_path).expect("run state");
    assert_eq!(
        run.bundle_repo_relative.as_deref(),
        Some(bundle.strip_prefix(repo_root).expect("relative bundle"))
    );
    assert_eq!(
        run.execution_state_repo_relative.as_deref(),
        Some(
            execution_state
                .strip_prefix(repo_root)
                .expect("relative execution state")
        )
    );
}

#[test]
fn tracking_run_init_rejects_source_paths_outside_verified_repository() {
    let tmp = TempDir::new().expect("tmp");
    let checkout = tmp.path().join("checkout");
    let external_bundle = tmp.path().join("external-bundle");
    fs::create_dir(&checkout).expect("checkout");
    fs::create_dir(&external_bundle).expect("external bundle");
    let execution_state = external_bundle.join("external-execution-state.md");
    fs::write(&execution_state, "## Task Ledger\n").expect("external execution state");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout)
            .status()
            .expect("git init")
            .success()
    );
    let run_state_path = checkout.join("run-state.json");
    let events_path = checkout.join("events.jsonl");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "local:demo",
            "--issue",
            "1271",
            "--bundle",
            external_bundle.to_str().expect("bundle path"),
            "--execution-state-file",
            execution_state.to_str().expect("execution-state path"),
            "--now",
            "2026-07-18T00:00:00Z",
            "--run-id",
            "external-source-run",
            "--out",
            run_state_path.to_str().expect("run-state path"),
        ],
        common::plan_issue_cmd_options().with_cwd(&checkout),
    );

    assert_ne!(out.code, 0, "stdout: {}", out.stdout_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-run-init-source-path-invalid"
    );
    assert!(
        !run_state_path.exists(),
        "invalid source must not write run-state"
    );
    assert!(
        !events_path.exists(),
        "invalid source must not append events"
    );
}

#[test]
fn tracking_run_init_dry_run_preserves_existing_state() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let events_path = tmp.path().join("events.jsonl");

    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "existing-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "reviewing",
        "--validation-overall",
        "pass",
        "--note",
        "retained accumulated state",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(update.code, 0, "update stderr: {}", update.stderr_text());

    let state_before = fs::read(&run_state_path).expect("state before");
    let events_before = fs::read(&events_path).expect("events before");
    let preview = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:02:00Z",
        "--run-id",
        "dry-preview",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(preview.code, 0, "preview stderr: {}", preview.stderr_text());

    let envelope = preview.stdout_json();
    assert_eq!(envelope["payload"]["dry_run"], true);
    let result = &envelope["payload"]["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["run_id"], "dry-preview");
    assert_eq!(
        result["run_state_path"],
        run_state_path.to_str().expect("path")
    );
    assert_eq!(result["events_path"], events_path.to_str().expect("path"));
    assert_eq!(
        fs::read(&run_state_path).expect("state after"),
        state_before,
        "dry-run must not rewrite accumulated run state"
    );
    assert_eq!(
        fs::read(&events_path).expect("events after"),
        events_before,
        "dry-run must not append run_started"
    );
}

#[test]
fn tracking_run_init_dry_run_does_not_create_missing_layout() {
    let tmp = TempDir::new().expect("tmp");
    let run_dir = tmp.path().join("missing-run");
    let run_state_path = run_dir.join("run-state.json");
    let events_path = run_dir.join("events.jsonl");

    let preview = common::run_plan_issue(&[
        "--format",
        "json",
        "--dry-run",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "missing-preview",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(preview.code, 0, "preview stderr: {}", preview.stderr_text());

    let envelope = preview.stdout_json();
    assert_eq!(envelope["payload"]["dry_run"], true);
    let result = &envelope["payload"]["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["run_id"], "missing-preview");
    assert_eq!(
        result["run_state_path"],
        run_state_path.to_str().expect("path")
    );
    assert_eq!(result["events_path"], events_path.to_str().expect("path"));
    assert!(
        !run_dir.exists(),
        "dry-run must not create its planned output directory"
    );
}

#[test]
fn tracking_run_init_dry_run_does_not_create_canonical_layout() {
    let tmp = TempDir::new().expect("tmp");
    let state_home = tmp.path().join("missing-state-home");
    let options = common::plan_issue_cmd_options().with_env(
        "PLAN_ISSUE_HOME",
        state_home.to_str().expect("state home path"),
    );

    let preview = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "owner/repo",
            "--issue",
            "123",
            "--now",
            "2026-05-26T00:00:00Z",
            "--run-id",
            "canonical-preview",
        ],
        options,
    );
    assert_eq!(preview.code, 0, "preview stderr: {}", preview.stderr_text());

    let envelope = preview.stdout_json();
    assert_eq!(envelope["payload"]["dry_run"], true);
    let result = &envelope["payload"]["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["run_id"], "canonical-preview");
    let run_state_path =
        std::path::PathBuf::from(result["run_state_path"].as_str().expect("run_state_path"));
    let events_path =
        std::path::PathBuf::from(result["events_path"].as_str().expect("events_path"));
    let run_root = run_state_path.parent().expect("run root");
    assert_eq!(events_path.parent(), Some(run_root));
    assert_eq!(
        run_state_path.file_name().and_then(|v| v.to_str()),
        Some("run-state.json")
    );
    assert_eq!(
        events_path.file_name().and_then(|v| v.to_str()),
        Some("events.jsonl")
    );
    assert!(run_state_path.starts_with(&state_home));
    assert!(
        !state_home.exists(),
        "dry-run must not create the canonical state root"
    );
    for child in ["inputs", "rendered", "artifacts"] {
        assert!(
            !run_root.join(child).exists(),
            "dry-run must not create canonical {child} directory"
        );
    }
}

#[test]
fn tracking_run_init_rejects_unsafe_run_id_before_writes() {
    let tmp = TempDir::new().expect("tmp");
    let state_home = tmp.path().join("missing-state-home");
    let escaped_run = tmp.path().join("escaped-run");
    let options = common::plan_issue_cmd_options().with_env(
        "PLAN_ISSUE_HOME",
        state_home.to_str().expect("state home path"),
    );

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "owner/repo",
            "--issue",
            "123",
            "--now",
            "2026-05-26T00:00:00Z",
            "--run-id",
            escaped_run.to_str().expect("escaped run path"),
        ],
        options,
    );
    assert_eq!(out.code, 1, "stdout: {}", out.stdout_text());
    let envelope = out.stdout_json();
    assert_eq!(envelope["error"]["code"], "tracking-run-init-layout-failed");
    assert!(
        envelope["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid run id")),
        "{}",
        out.stdout_text()
    );
    assert!(
        !state_home.exists(),
        "unsafe run id must fail before creating the canonical state root"
    );
    assert!(
        !escaped_run.exists(),
        "unsafe run id must fail before creating an escaped run root"
    );

    let explicit_out = tmp.path().join("explicit-output/run-state.json");
    let explicit = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "owner/repo",
            "--issue",
            "123",
            "--now",
            "2026-05-26T00:00:00Z",
            "--run-id",
            "../escape",
            "--out",
            explicit_out.to_str().expect("explicit out path"),
        ],
        common::plan_issue_cmd_options().with_env(
            "PLAN_ISSUE_HOME",
            state_home.to_str().expect("state home path"),
        ),
    );
    assert_eq!(explicit.code, 1, "stdout: {}", explicit.stdout_text());
    assert!(
        !explicit_out.parent().expect("explicit parent").exists(),
        "unsafe run id must be rejected even when --out supplies the output path"
    );
}

#[test]
fn tracking_run_init_rejects_unsafe_provider_repo_before_writes() {
    let tmp = TempDir::new().expect("tmp");
    let state_home = tmp.path().join("missing-state-home");
    let escaped_issue_root = state_home.join("out/issue-123");
    let options = common::plan_issue_cmd_options().with_env(
        "PLAN_ISSUE_HOME",
        state_home.to_str().expect("state home path"),
    );

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "..",
            "--issue",
            "123",
            "--now",
            "2026-05-26T00:00:00Z",
            "--run-id",
            "safe-run",
        ],
        options,
    );
    assert_eq!(out.code, 64, "stdout: {}", out.stdout_text());
    let envelope = out.stdout_json();
    assert_eq!(
        envelope["error"]["code"],
        "tracking-run-init-repo-identity-invalid"
    );
    assert!(
        !state_home.exists(),
        "unsafe provider repo must fail before creating the state root"
    );
    assert!(
        !escaped_issue_root.exists(),
        "unsafe provider repo must fail before creating an escaped issue root"
    );
}

#[test]
fn tracking_run_init_leaves_unproven_bare_repository_unbound() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "owner/repo",
            "--issue",
            "123",
            "--now",
            "2026-07-18T00:00:00Z",
            "--run-id",
            "unbound-run",
            "--out",
            run_state_path.to_str().expect("path"),
        ],
        common::plan_issue_cmd_options().with_cwd(tmp.path()),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let value: Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path).expect("run state"))
            .expect("run json");
    assert_eq!(value["repo"], "owner/repo");
    assert!(value.get("repo_provider").is_none(), "{value}");
    assert!(value.get("repo_host").is_none(), "{value}");
}

#[test]
fn tracking_run_init_resolves_nested_bare_repo_from_qualified_global_identity() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://gitlab.example.test/group/subgroup/project.git",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "group/subgroup/project",
            "--issue",
            "123",
            "--now",
            "2026-07-18T00:00:00Z",
            "--run-id",
            "global-nested-bound-run",
            "--out",
            run_state_path.to_str().expect("path"),
        ],
        common::plan_issue_cmd_options().with_cwd(tmp.path()),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let value: Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path).expect("run state"))
            .expect("run json");
    assert_eq!(value["repo"], "group/subgroup/project");
    assert_eq!(value["repo_provider"], "gitlab");
    assert_eq!(value["repo_host"], "gitlab.example.test");
}

#[test]
fn tracking_run_init_uses_matching_qualified_global_repository_binding() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://gitlab.com/owner/repo.git",
            "tracking",
            "run",
            "init",
            "--provider-repo",
            "owner/repo",
            "--issue",
            "123",
            "--now",
            "2026-07-18T00:00:00Z",
            "--run-id",
            "global-bound-run",
            "--out",
            run_state_path.to_str().expect("path"),
        ],
        common::plan_issue_cmd_options().with_cwd(tmp.path()),
    );
    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );

    let value: Value =
        serde_json::from_str(&fs::read_to_string(&run_state_path).expect("run state"))
            .expect("run json");
    assert_eq!(value["repo"], "owner/repo");
    assert_eq!(value["repo_provider"], "gitlab");
    assert_eq!(value["repo_host"], "gitlab.com");
}

#[test]
fn tracking_run_init_rejects_invalid_provider_repo_as_usage_error() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "not-a-repository",
        "--issue",
        "123",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);

    assert_eq!(out.code, 64, "stderr: {}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "tracking-run-init-repo-identity-invalid"
    );
    assert!(!run_state_path.exists());
}

#[test]
fn tracking_run_init_defaults_now_to_wallclock_when_now_omitted() {
    // Regression (issue #588): omitting `--now` must not write the 1970 epoch
    // placeholder into live run-state. The safe default is the current UTC time,
    // and `run_id` is derived from it rather than the `00000000…` placeholder.
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_ne!(
        run.created_at, "1970-01-01T00:00:00Z",
        "init without --now must not record the 1970 placeholder"
    );
    assert_eq!(run.created_at, run.updated_at, "init seeds both timestamps");
    // RFC3339 UTC form with `Z` suffix, matching the workspace convention.
    assert!(
        run.created_at.contains('T') && run.created_at.ends_with('Z'),
        "created_at should be RFC3339 UTC: {}",
        run.created_at
    );

    let envelope = out.stdout_json();
    let run_id = envelope["payload"]["result"]["run_id"]
        .as_str()
        .expect("run_id");
    assert!(
        !run_id.starts_with("00000000000000"),
        "run_id should derive from a real timestamp, not the placeholder: {run_id}"
    );
    assert!(run_id.ends_with("-issue-123"), "run_id: {run_id}");
}

#[test]
fn tracking_run_update_changes_phase_and_appends_event() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    // Initialize.
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-update",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "init stderr: {}", out.stderr_text());

    // Update phase + validation.
    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "validating",
        "--validation-overall",
        "pass",
        "--validation-command",
        "cargo test",
        "--validation-status",
        "pass",
        "--note",
        "validated locally",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(out.code, 0, "update stderr: {}", out.stderr_text());
    let envelope = out.stdout_json();
    let result = &envelope["payload"]["result"];
    assert_eq!(result["phase"], "validating");
    let changed: Vec<&str> = result["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(changed.contains(&"phase"));
    assert!(changed.contains(&"validation"));
    assert!(changed.contains(&"note"));

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "validating");
    assert_eq!(
        run.validation.as_ref().map(|v| v.overall.clone()),
        Some("pass".to_string())
    );
    assert!(!run.notes.is_empty());
    let events = std::fs::read_to_string(run_state_path.parent().unwrap().join("events.jsonl"))
        .expect("events");
    assert!(events.contains("run_updated"));
}

#[test]
fn tracking_run_update_closed_clears_stale_selected_task() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "2.3",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-closed",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "closed",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(update.code, 0, "update stderr: {}", update.stderr_text());
    let result = &update.stdout_json()["payload"]["result"];
    assert_eq!(
        result["changed"],
        serde_json::json!(["phase", "selected_task"])
    );

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "closed");
    assert_eq!(
        run.selected_scope
            .as_ref()
            .and_then(|scope| scope.task.as_deref()),
        None
    );
    let events =
        fs::read_to_string(run_state_path.parent().unwrap().join("events.jsonl")).expect("events");
    let last_event: Value =
        serde_json::from_str(events.lines().last().expect("last event")).expect("event json");
    assert_eq!(
        last_event["detail"]["changed"],
        serde_json::json!(["phase", "selected_task"])
    );
}

#[test]
fn tracking_run_update_rejects_selected_task_when_closing() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");

    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "2.3",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-closed-task-rejected",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "closed",
        "--selected-task",
        "9.9",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_ne!(update.code, 0, "stdout: {}", update.stdout_text());

    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "initial");
    assert_eq!(
        run.selected_scope
            .as_ref()
            .and_then(|scope| scope.task.as_deref()),
        Some("2.3")
    );
}

#[test]
fn tracking_run_update_rejects_selected_task_for_already_closed_run() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "2.3",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-already-closed",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());
    let close = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "closed",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(close.code, 0, "close stderr: {}", close.stderr_text());

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--selected-task",
        "9.9",
        "--now",
        "2026-05-26T00:02:00Z",
    ]);
    assert_ne!(update.code, 0, "stdout: {}", update.stdout_text());
    assert_eq!(
        update.stdout_json()["error"]["code"],
        "tracking-run-update-closed-selected-task"
    );
    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "closed");
    assert_eq!(
        run.selected_scope
            .as_ref()
            .and_then(|scope| scope.task.as_deref()),
        None
    );
}

#[test]
fn tracking_run_update_rejects_all_ordinary_mutations_after_close() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let events_path = tmp.path().join("events.jsonl");
    let raw = serde_json::json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "immutable-closed-run",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "closed",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T00:01:00Z"
    });
    fs::write(&run_state_path, raw.to_string()).expect("closed run-state");
    fs::write(&events_path, "seed-event\n").expect("seed events");
    let state_before = fs::read(&run_state_path).expect("state before");
    let events_before = fs::read(&events_path).expect("events before");

    let cases: &[(&str, &[&str])] = &[
        ("branch", &["--branch", "post-close"]),
        ("linked-pr", &["--linked-pr", "owner/repo#99"]),
        ("validation", &["--validation-overall", "pass"]),
        ("review", &["--review-decision", "approve"]),
        ("note", &["--note", "post-close note"]),
    ];

    for (name, mutation_args) in cases {
        let mut args = vec![
            "--format",
            "json",
            "tracking",
            "run",
            "update",
            "--run-state",
            run_state_path.to_str().expect("path"),
        ];
        args.extend_from_slice(mutation_args);
        args.extend_from_slice(&["--now", "2026-05-26T00:02:00Z"]);
        let update = common::run_plan_issue(&args);

        assert_ne!(update.code, 0, "{name}: {}", update.stdout_text());
        assert_eq!(
            update.stdout_json()["error"]["code"],
            "tracking-run-update-closed-immutable",
            "{name}: stdout={} stderr={}",
            update.stdout_text(),
            update.stderr_text()
        );
        assert_eq!(
            fs::read(&run_state_path).expect("state after"),
            state_before,
            "{name} must not rewrite closed state"
        );
        assert_eq!(
            fs::read(&events_path).expect("events after"),
            events_before,
            "{name} must not append an event"
        );
    }
}

#[test]
fn tracking_run_update_closed_no_ops_do_not_rewrite_state_or_events() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let events_path = tmp.path().join("events.jsonl");
    let raw = serde_json::json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "clean-closed-run",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "closed",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T00:01:00Z"
    });
    fs::write(&run_state_path, raw.to_string()).expect("closed run-state");
    fs::write(&events_path, "seed-event\n").expect("seed events");
    let state_before = fs::read(&run_state_path).expect("state before");
    let events_before = fs::read(&events_path).expect("events before");

    for extra_args in [Vec::new(), vec!["--phase", "closed"]] {
        let mut args = vec![
            "--format",
            "json",
            "tracking",
            "run",
            "update",
            "--run-state",
            run_state_path.to_str().expect("path"),
        ];
        args.extend(extra_args);
        args.extend(["--now", "2026-05-26T00:02:00Z"]);
        let update = common::run_plan_issue(&args);

        assert_eq!(update.code, 0, "stderr: {}", update.stderr_text());
        assert_eq!(
            update.stdout_json()["payload"]["result"]["changed"],
            serde_json::json!([])
        );
        assert_eq!(
            fs::read(&run_state_path).expect("state after"),
            state_before,
            "a clean closed run must remain byte-identical"
        );
        assert_eq!(
            fs::read(&events_path).expect("events after"),
            events_before,
            "a clean closed run must not append events"
        );
    }
}

#[test]
fn tracking_run_update_idempotent_closed_repairs_legacy_stale_task() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let raw = serde_json::json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "historical-closed",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "closed",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T00:01:00Z",
        "selected_scope": {"task": "stale"}
    });
    fs::write(&run_state_path, raw.to_string()).expect("historical run-state");

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "closed",
        "--now",
        "2026-05-26T00:02:00Z",
    ]);
    assert_eq!(update.code, 0, "stderr: {}", update.stderr_text());
    assert_eq!(
        update.stdout_json()["payload"]["result"]["changed"],
        serde_json::json!(["selected_task"])
    );
    let run = run_state::read_run_state(&run_state_path).expect("read");
    assert_eq!(run.phase.as_str(), "closed");
    assert_eq!(
        run.selected_scope
            .as_ref()
            .and_then(|scope| scope.task.as_deref()),
        None
    );
}

#[test]
fn tracking_run_update_rejects_transition_out_of_closed() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let raw = serde_json::json!({
        "schema": "plan-issue.execution-run.v1",
        "run_id": "closed-run",
        "repo": "owner/repo",
        "issue": 123,
        "profile": "tracking",
        "phase": "closed",
        "created_at": "2026-05-26T00:00:00Z",
        "updated_at": "2026-05-26T00:01:00Z"
    });
    fs::write(&run_state_path, raw.to_string()).expect("closed run-state");

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "implementing",
        "--now",
        "2026-05-26T00:02:00Z",
    ]);
    assert_ne!(update.code, 0, "stdout: {}", update.stdout_text());
    assert_eq!(
        update.stdout_json()["error"]["code"],
        "tracking-run-update-closed-transition"
    );
    assert_eq!(
        run_state::read_run_state(&run_state_path)
            .expect("read")
            .phase
            .as_str(),
        "closed"
    );
}

#[test]
fn tracking_run_update_records_rich_review_evidence() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let findings_path = tmp.path().join("review-findings.json");
    fs::write(
        &findings_path,
        r#"[{"id":"F1","severity":"minor","disposition":"fixed","summary":"Review context renders visibly"}]"#,
    )
    .expect("findings");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "run-review",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(out.code, 0, "init stderr: {}", out.stderr_text());

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--review-decision",
        "approve",
        "--review-lens",
        "testing",
        "--review-lens",
        "maintainability",
        "--review-outcome-comment",
        "https://example.test/review",
        "--review-findings-file",
        findings_path.to_str().expect("findings"),
        "--now",
        "2026-05-26T00:02:00Z",
    ]);
    assert_eq!(out.code, 0, "update stderr: {}", out.stderr_text());

    let run = run_state::read_run_state(&run_state_path).expect("read");
    let review = run.review.expect("review summary");
    assert_eq!(review.decision, "approve");
    assert_eq!(review.lenses, vec!["testing", "maintainability"]);
    assert_eq!(
        review.evidence.as_deref(),
        Some("https://example.test/review")
    );
    assert_eq!(review.findings.len(), 1);
    assert_eq!(review.findings[0].id, "F1");
    assert_eq!(review.findings[0].disposition, "fixed");
}

#[test]
fn tracking_run_update_rejects_invalid_run_state() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    fs::write(&run_state_path, "not json").expect("write");

    let out = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "validating",
    ]);
    assert_ne!(out.code, 0, "should fail");
    let envelope = out.stdout_json();
    assert_eq!(envelope["error"]["code"], "tracking-run-update-read-failed");
}

#[test]
fn tracking_run_update_fails_fast_while_same_run_is_locked() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "locked-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let lock_path = tmp.path().join(".run-state.json.update.lock");
    let _active_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&lock_path)
        .expect("hold advisory run lock");
    let state_before = fs::read(&run_state_path).expect("state before");
    let events_path = tmp.path().join("events.jsonl");
    let events_before = fs::read(&events_path).expect("events before");

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "closed",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);

    assert_ne!(update.code, 0, "stdout: {}", update.stdout_text());
    assert_eq!(
        update.stdout_json()["error"]["code"],
        "tracking-run-update-lock-busy"
    );
    assert_eq!(
        fs::read(&run_state_path).expect("state after"),
        state_before
    );
    assert_eq!(fs::read(&events_path).expect("events after"), events_before);
}

#[cfg(unix)]
#[test]
fn tracking_run_update_rejects_symlink_and_hard_link_aliases_without_mutation() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "alias-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());
    let state_before = fs::read(&run_state_path).expect("state before");
    let events_path = tmp.path().join("events.jsonl");
    let events_before = fs::read(&events_path).expect("events before");

    let symlink_path = tmp.path().join("run-state-symlink.json");
    symlink(&run_state_path, &symlink_path).expect("symlink alias");
    let hard_link_path = tmp.path().join("run-state-hard-link.json");
    fs::hard_link(&run_state_path, &hard_link_path).expect("hard-link alias");

    for alias in [&symlink_path, &hard_link_path] {
        let update = common::run_plan_issue(&[
            "--format",
            "json",
            "tracking",
            "run",
            "update",
            "--run-state",
            alias.to_str().expect("alias path"),
            "--phase",
            "validating",
            "--now",
            "2026-05-26T00:01:00Z",
        ]);

        assert_eq!(
            update.code,
            1,
            "alias={} stdout={}",
            alias.display(),
            update.stdout_text()
        );
        assert_eq!(
            update.stdout_json()["error"]["code"],
            "tracking-run-update-target-unsafe",
            "alias={}",
            alias.display()
        );
        assert_eq!(
            fs::read(&run_state_path).expect("state after"),
            state_before
        );
        assert_eq!(fs::read(&events_path).expect("events after"), events_before);
    }
}

#[cfg(unix)]
#[test]
fn tracking_run_update_atomically_replaces_run_state_generation() {
    use std::os::unix::fs::MetadataExt;

    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "atomic-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());
    let inode_before = fs::metadata(&run_state_path)
        .expect("metadata before")
        .ino();

    let update = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--phase",
        "validating",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(update.code, 0, "update stderr: {}", update.stderr_text());

    let inode_after = fs::metadata(&run_state_path).expect("metadata after").ino();
    assert_ne!(
        inode_after, inode_before,
        "atomic replace must publish a new inode"
    );
    let run = run_state::read_run_state(&run_state_path).expect("valid replacement state");
    assert_eq!(run.phase.as_str(), "validating");
}

#[test]
fn tracking_run_update_same_open_values_are_semantic_no_ops() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--task",
        "1.2",
        "--branch",
        "feat/same",
        "--linked-pr",
        "owner/repo#7",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "same-values-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());
    let first = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--validation-overall",
        "pass",
        "--validation-command",
        "cargo test",
        "--validation-status",
        "pass",
        "--validation-evidence",
        "evidence.log",
        "--review-decision",
        "approve",
        "--review-lens",
        "testing",
        "--review-outcome-comment",
        "https://example.test/review",
        "--now",
        "2026-05-26T00:01:00Z",
    ]);
    assert_eq!(
        first.code,
        0,
        "first update stderr: {}",
        first.stderr_text()
    );

    let state_before = fs::read(&run_state_path).expect("state before");
    let events_path = tmp.path().join("events.jsonl");
    let events_before = fs::read(&events_path).expect("events before");
    let second = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--selected-task",
        "1.2",
        "--branch",
        "feat/same",
        "--linked-pr",
        "owner/repo#7",
        "--validation-overall",
        "pass",
        "--validation-command",
        "cargo test",
        "--validation-status",
        "pass",
        "--validation-evidence",
        "evidence.log",
        "--review-decision",
        "approve",
        "--review-lens",
        "testing",
        "--review-outcome-comment",
        "https://example.test/review",
        "--now",
        "2026-05-26T00:02:00Z",
    ]);

    assert_eq!(
        second.code,
        0,
        "second update stderr: {}",
        second.stderr_text()
    );
    assert_eq!(
        second.stdout_json()["payload"]["result"]["changed"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read(&run_state_path).expect("state after"),
        state_before
    );
    assert_eq!(fs::read(&events_path).expect("events after"), events_before);
}

#[test]
fn tracking_run_update_recovers_event_append_after_state_write_on_identical_retry() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "recover-event-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let events_path = tmp.path().join("events.jsonl");
    fs::remove_file(&events_path).expect("remove initial event file");
    fs::create_dir(&events_path).expect("block event append with directory");
    let update_args = [
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--branch",
        "feat/recover-event",
        "--now",
        "2026-05-26T00:01:00Z",
    ];

    let first = common::run_plan_issue(&update_args);
    assert_ne!(
        first.code,
        0,
        "first update must fail: {}",
        first.stdout_text()
    );
    assert_eq!(
        first.stdout_json()["error"]["code"],
        "tracking-run-update-event-append-failed"
    );
    assert_eq!(
        run_state::read_run_state(&run_state_path)
            .expect("state written before append failure")
            .branch
            .as_deref(),
        Some("feat/recover-event")
    );

    fs::remove_dir(&events_path).expect("unblock event append");
    let retry = common::run_plan_issue(&update_args);
    assert_eq!(
        retry.code,
        0,
        "identical retry must recover: stdout={} stderr={}",
        retry.stdout_text(),
        retry.stderr_text()
    );
    assert_eq!(
        retry.stdout_json()["payload"]["result"]["changed"],
        serde_json::json!(["branch"]),
        "recovery must report the durable mutation whose event was repaired"
    );
    let events = fs::read_to_string(&events_path).expect("recovered events");
    assert_eq!(
        events
            .lines()
            .filter(|line| line.contains("\"type\":\"run_updated\""))
            .count(),
        1,
        "retry must produce exactly one event: {events}"
    );
    assert!(
        !tmp.path()
            .join(".run-state.json.pending-update-event.json")
            .exists(),
        "successful recovery must clear its pending journal"
    );
}

#[test]
fn tracking_run_update_rejects_incomplete_validation_groups_without_mutation() {
    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "validation-groups-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());
    let events_path = tmp.path().join("events.jsonl");
    let state_before = fs::read(&run_state_path).expect("state before");
    let events_before = fs::read(&events_path).expect("events before");
    let cases: &[(&str, &[&str], &str)] = &[
        (
            "command-only",
            &["--validation-command", "cargo test"],
            "tracking-run-update-validation-command-status-required",
        ),
        (
            "status-only",
            &["--validation-status", "pass"],
            "tracking-run-update-validation-command-status-required",
        ),
        (
            "command-and-evidence",
            &[
                "--validation-command",
                "cargo test",
                "--validation-evidence",
                "test.log",
            ],
            "tracking-run-update-validation-command-status-required",
        ),
        (
            "evidence-only",
            &["--validation-evidence", "test.log"],
            "tracking-run-update-validation-evidence-requires-command",
        ),
        (
            "status-and-evidence",
            &[
                "--validation-status",
                "pass",
                "--validation-evidence",
                "test.log",
            ],
            "tracking-run-update-validation-evidence-requires-command",
        ),
    ];

    for (name, validation_args, expected_code) in cases {
        let mut args = vec![
            "--format",
            "json",
            "tracking",
            "run",
            "update",
            "--run-state",
            run_state_path.to_str().expect("path"),
        ];
        args.extend_from_slice(validation_args);
        args.extend_from_slice(&["--now", "2026-05-26T00:01:00Z"]);
        let out = common::run_plan_issue(&args);

        assert_eq!(out.code, 64, "{name}: stdout={}", out.stdout_text());
        assert_eq!(out.stdout_json()["error"]["code"], *expected_code, "{name}");
        assert_eq!(
            fs::read(&run_state_path).expect("state after"),
            state_before,
            "{name}: incomplete validation arguments mutated state"
        );
        assert_eq!(
            fs::read(&events_path).expect("events after"),
            events_before,
            "{name}: incomplete validation arguments appended an event"
        );
    }
}

#[test]
fn malformed_repository_credentials_are_redacted_at_cli_boundary() {
    let cases = [
        (
            "provider-repo",
            vec![
                "--format",
                "json",
                "tracking",
                "run",
                "init",
                "--provider-repo",
                "https://boundary-user:boundary-password@internal.ghe.com:not-a-port/acme/widgets",
                "--issue",
                "123",
            ],
        ),
        (
            "global-repo",
            vec![
                "--format",
                "json",
                "--repo",
                "https://boundary-user:boundary-password@internal.ghe.com:not-a-port/acme/widgets",
                "tracking",
                "run",
                "init",
                "--provider-repo",
                "acme/widgets",
                "--issue",
                "123",
            ],
        ),
    ];

    for (name, args) in cases {
        let tmp = TempDir::new().expect("tmp");
        let out = common::run_plan_issue_with_options(
            &args,
            common::plan_issue_cmd_options().with_cwd(tmp.path()),
        );
        assert_eq!(out.code, 64, "{name}: stdout={}", out.stdout_text());
        let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
        for secret in [
            "boundary-user",
            "boundary-password",
            "https://boundary-user:boundary-password@internal.ghe.com:not-a-port/acme/widgets",
        ] {
            assert!(
                !combined.contains(secret),
                "{name} leaked {secret:?}: {combined}"
            );
        }
    }
}

#[test]
fn malformed_credential_bearing_origin_is_redacted_at_cli_boundary() {
    let tmp = TempDir::new().expect("tmp");
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status()
            .expect("git init")
            .success()
    );
    let raw_remote =
        "https://origin-user:origin-token@internal.ghe.com:not-a-port/acme/widgets.git";
    assert!(
        std::process::Command::new("git")
            .args(["remote", "add", "origin", raw_remote])
            .current_dir(tmp.path())
            .status()
            .expect("git remote add")
            .success()
    );

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "record",
            "repair-dashboard",
            "--issue",
            "42",
        ],
        common::plan_issue_cmd_options().with_cwd(tmp.path()),
    );
    assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
    let combined = format!("{}{}", out.stdout_text(), out.stderr_text());
    for secret in ["origin-user", "origin-token", raw_remote] {
        assert!(
            !combined.contains(secret),
            "origin leaked {secret:?}: {combined}"
        );
    }
}

#[cfg(unix)]
#[test]
fn tracking_run_update_lock_serializes_distinct_mutations_without_lost_success() {
    use std::io::Write;
    use std::time::{Duration, Instant};

    let tmp = TempDir::new().expect("tmp");
    let run_state_path = tmp.path().join("run-state.json");
    let init = common::run_plan_issue(&[
        "--format",
        "json",
        "tracking",
        "run",
        "init",
        "--provider-repo",
        "owner/repo",
        "--issue",
        "123",
        "--now",
        "2026-05-26T00:00:00Z",
        "--run-id",
        "serialized-run",
        "--out",
        run_state_path.to_str().expect("path"),
    ]);
    assert_eq!(init.code, 0, "init stderr: {}", init.stderr_text());

    let findings_fifo = tmp.path().join("findings.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&findings_fifo)
            .status()
            .expect("mkfifo")
            .success()
    );
    let first = std::process::Command::new(env!("CARGO_BIN_EXE_plan-issue"))
        .args([
            "--format",
            "json",
            "tracking",
            "run",
            "update",
            "--run-state",
            run_state_path.to_str().expect("path"),
            "--review-decision",
            "approve",
            "--review-findings-file",
            findings_fifo.to_str().expect("fifo"),
            "--now",
            "2026-05-26T00:01:00Z",
        ])
        .env("PLAN_ISSUE_HOME", tmp.path().join("state-home"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn first update");
    let lock_path = tmp.path().join(".run-state.json.update.lock");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut first_holds_lock = false;
    while Instant::now() < deadline {
        match plan_tooling::mutation_lock::OwnedFileLock::acquire(&lock_path) {
            Err(plan_tooling::mutation_lock::OwnedFileLockError::Busy) => {
                first_holds_lock = true;
                break;
            }
            Ok(lock) => {
                drop(lock);
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(plan_tooling::mutation_lock::OwnedFileLockError::Failed(err)) => {
                panic!("probe run lock: {err}");
            }
        }
    }
    assert!(first_holds_lock, "first update did not acquire lock");

    let second_args = [
        "--format",
        "json",
        "tracking",
        "run",
        "update",
        "--run-state",
        run_state_path.to_str().expect("path"),
        "--branch",
        "feat/second",
        "--now",
        "2026-05-26T00:02:00Z",
    ];
    let blocked = common::run_plan_issue(&second_args);
    assert_eq!(blocked.code, 1, "stdout={}", blocked.stdout_text());
    assert_eq!(
        blocked.stdout_json()["error"]["code"],
        "tracking-run-update-lock-busy"
    );

    let mut fifo = fs::OpenOptions::new()
        .write(true)
        .open(&findings_fifo)
        .expect("open fifo writer");
    fifo.write_all(b"[]").expect("unblock findings read");
    drop(fifo);
    let first_output = first.wait_with_output().expect("wait for first update");
    assert!(
        first_output.status.success(),
        "first stdout={} stderr={}",
        String::from_utf8_lossy(&first_output.stdout),
        String::from_utf8_lossy(&first_output.stderr)
    );

    let retry = common::run_plan_issue(&second_args);
    assert_eq!(retry.code, 0, "retry stderr={}", retry.stderr_text());
    let run = run_state::read_run_state(&run_state_path).expect("final run state");
    assert_eq!(run.branch.as_deref(), Some("feat/second"));
    assert_eq!(
        run.review.as_ref().map(|review| review.decision.as_str()),
        Some("approve")
    );
    let events = fs::read_to_string(tmp.path().join("events.jsonl")).expect("events");
    assert_eq!(
        events
            .lines()
            .filter(|line| line.contains("\"type\":\"run_updated\""))
            .count(),
        2,
        "each successful distinct mutation must retain one event: {events}"
    );
}

#[test]
fn tracking_run_update_help_lists_run_init_and_update() {
    let out = common::run_plan_issue(&["tracking", "run", "--help"]);
    assert_eq!(out.code, 0);
    assert!(out.stdout_text().contains("init"));
    assert!(out.stdout_text().contains("update"));
}
