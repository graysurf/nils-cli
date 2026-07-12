use std::fs;
use std::path::Path;

use nils_test_support::cmd::{CmdOutput, run_resolved_in_dir};
use pretty_assertions::assert_eq;

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved_in_dir("test-first-evidence", dir, args, &[], None)
}

fn out_arg(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn combined_output(output: &CmdOutput) -> String {
    format!("{}{}", output.stdout_text(), output.stderr_text())
}

#[test]
fn help_includes_version_flag_and_examples() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["--help"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let stdout = output.stdout_text();
    assert!(
        stdout.contains("-V, --version"),
        "missing version flag: {stdout}"
    );
    assert!(stdout.contains("EXAMPLES:"), "missing examples: {stdout}");
}

#[test]
fn records_durable_v2_lifecycle_and_appends_scoped_evidence() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("evidence");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
            "--production-path",
            "src/lib.rs",
            "--changed-behavior",
            "verification requires durable test evidence",
            "--invariant",
            "v1 records remain readable",
            "--note",
            "ticket VY-1",
            "--format",
            "json",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());
    let init_json = init.stdout_json();
    assert_eq!(
        init_json["schema_version"],
        "cli.test-first-evidence.init.v2"
    );
    assert_eq!(init_json["command"], "test-first-evidence init");
    assert_eq!(init_json["ok"], true);
    assert_eq!(init_json["result"]["complete"], false);

    assert_eq!(
        init_json["result"]["record"]["schema_version"],
        "test-first-evidence.record.v2"
    );

    let impact = run(
        tmp.path(),
        &[
            "record-impact",
            "--out",
            &out_arg,
            "--target",
            "tests/test_first.rs::durable_contract",
            "--disposition",
            "update-spec",
            "--protected-behavior",
            "durable verification contract",
            "--reason",
            "the v1 expectation represents the old specification",
            "--owner-test",
            "durable_contract",
            "--validation-scope",
            "affected-suite",
            "--format",
            "json",
        ],
    );
    assert_eq!(impact.code, 0, "stderr={}", impact.stderr_text());
    let duplicate_impact = run(
        tmp.path(),
        &[
            "record-impact",
            "--out",
            &out_arg,
            "--target",
            "tests/test_first.rs::durable_contract",
            "--disposition",
            "update-spec",
            "--protected-behavior",
            "durable verification contract",
            "--reason",
            "duplicate fixture",
            "--owner-test",
            "durable_contract",
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate_impact.code, 65);
    assert_eq!(
        duplicate_impact.stdout_json()["error"]["code"],
        "duplicate-test-impact"
    );

    for (test_name, observed_failure) in [
        ("durable_contract", "record schema was v1"),
        (
            "append_contract",
            "second failing record replaced the first",
        ),
    ] {
        let failing = run(
            tmp.path(),
            &[
                "record-failing",
                "--out",
                &out_arg,
                "--command",
                &format!("cargo test {test_name}"),
                "--exit-code",
                "101",
                "--summary",
                "assertion failed before fix",
                "--test-name",
                test_name,
                "--expected-failure",
                "durable v2 behavior is not implemented",
                "--observed-failure",
                observed_failure,
                "--format",
                "json",
            ],
        );
        assert_eq!(failing.code, 0, "stderr={}", failing.stderr_text());
    }

    let gap = run(tmp.path(), &["record-gap", "--out", &out_arg, "--none"]);
    assert_eq!(gap.code, 0, "stderr={}", gap.stderr_text());

    for (scope, command) in [
        ("focused", "cargo test durable_contract"),
        ("affected-suite", "cargo test test_first_evidence"),
    ] {
        let final_validation = run(
            tmp.path(),
            &[
                "record-final",
                "--out",
                &out_arg,
                "--command",
                command,
                "--status",
                "pass",
                "--scope",
                scope,
                "--summary",
                "durable evidence validation passed",
                "--format",
                "json",
            ],
        );
        assert_eq!(
            final_validation.code,
            0,
            "stderr={}",
            final_validation.stderr_text()
        );
    }

    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    let verify_json = verify.stdout_json();
    assert_eq!(
        verify_json["schema_version"],
        "cli.test-first-evidence.verify.v2"
    );
    assert_eq!(verify_json["result"]["complete"], true);
    assert_eq!(
        verify_json["result"]["record"]["failing_tests"]
            .as_array()
            .expect("failing tests")
            .len(),
        2
    );
    assert_eq!(
        verify_json["result"]["record"]["final_validations"]
            .as_array()
            .expect("final validations")
            .len(),
        2
    );

    let record = fs::read_to_string(out_dir.join("test-first-evidence.json")).expect("record");
    assert!(record.contains("\"schema_version\": \"test-first-evidence.record.v2\""));
    assert!(record.contains("\"status\": \"pass\""));
}

#[test]
fn waiver_path_can_complete_evidence_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("waiver-evidence");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &["init", "--out", &out_arg, "--classification", "docs-only"],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let waiver = run(
        tmp.path(),
        &[
            "record-waiver",
            "--out",
            &out_arg,
            "--reason",
            "docs-only change; no production behavior edited",
            "--waiver-kind",
            "non-testable",
            "--why-no-red",
            "no production contract changes",
            "--substitute-validation",
            "markdownlint passed",
        ],
    );
    assert_eq!(waiver.code, 0, "stderr={}", waiver.stderr_text());

    let final_validation = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            "bash scripts/ci/markdownlint-audit.sh --strict",
            "--status",
            "pass",
            "--scope",
            "manual",
        ],
    );

    let gap = run(tmp.path(), &["record-gap", "--out", &out_arg, "--none"]);
    assert_eq!(gap.code, 0, "stderr={}", gap.stderr_text());
    assert_eq!(
        final_validation.code,
        0,
        "stderr={}",
        final_validation.stderr_text()
    );

    let verify = run(tmp.path(), &["verify", "--out", &out_arg]);
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    assert!(
        verify
            .stdout_text()
            .contains("test-first evidence complete"),
        "missing completion text: {}",
        verify.stdout_text()
    );
}

#[test]
fn verify_incomplete_record_returns_json_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("incomplete");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
            "--changed-behavior",
            "new durable contract",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stdout={}", verify.stdout_text());
    let value = verify.stdout_json();
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "incomplete-evidence");
    let missing = value["error"]["details"]["missing"]
        .as_array()
        .expect("missing list");
    assert!(missing.iter().any(|value| value == "test_impacts_or_none"));
    assert!(
        missing
            .iter()
            .any(|value| value == "failing_tests_or_waiver")
    );
    assert!(
        missing
            .iter()
            .any(|value| value == "focused_final_validation")
    );
    assert!(
        missing
            .iter()
            .any(|value| value == "residual_gaps_declaration")
    );
}

#[test]
fn classification_and_contract_delta_fail_closed() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let unknown_dir = tmp.path().join("unknown");
    let unknown_arg = out_arg(&unknown_dir);
    let unknown = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &unknown_arg,
            "--classification",
            "behavior_change",
        ],
    );
    assert_eq!(unknown.code, 64, "stderr={}", unknown.stderr_text());

    let retained_dir = tmp.path().join("retained-only");
    let retained_arg = out_arg(&retained_dir);
    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &retained_arg,
            "--classification",
            "behavior-change",
            "--retained-behavior",
            "existing contract",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());
    let verify = run(
        tmp.path(),
        &["verify", "--out", &retained_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    assert!(
        verify.stdout_json()["error"]["details"]["missing"]
            .as_array()
            .expect("missing")
            .iter()
            .any(|item| item == "changed_added_or_removed_behavior")
    );
}

#[test]
fn strict_verify_rejects_invalid_raw_v2_fields() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("invalid-raw");
    fs::create_dir_all(&out_dir).expect("invalid raw dir");
    fs::write(
        out_dir.join("test-first-evidence.json"),
        r#"{
  "schema_version": "test-first-evidence.record.v2",
  "change_classification": "unknown-kind",
  "waiver": {
    "reason": "fixture",
    "kind": "non-testable",
    "why_no_red": "fixture",
    "substitute_validation": ["manual check"]
  },
  "final_validations": [
    {"command": "manual check", "status": "pass", "scope": "manual"}
  ],
  "no_residual_gaps": true
}"#,
    )
    .expect("unknown classification record");
    let out_arg = out_arg(&out_dir);
    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    assert!(
        verify.stdout_json()["error"]["details"]["missing"]
            .as_array()
            .expect("missing")
            .iter()
            .any(|item| item == "invalid_change_classification")
    );

    fs::write(
        out_dir.join("test-first-evidence.json"),
        r#"{
  "schema_version": "test-first-evidence.record.v2",
  "change_classification": "behavior-change",
  "contract_delta": {"changed_behaviors": ["new behavior"]},
  "test_impacts": [{
    "target": "tests::contract",
    "disposition": "invented",
    "protected_behavior": "contract",
    "reason": "fixture"
  }],
  "failing_tests": [{
    "command": "cargo test",
    "exit_code": 101,
    "summary": "red",
    "expected_failure": "missing behavior",
    "observed_failure": "assertion mismatch"
  }],
  "final_validations": [
    {"command": "cargo test", "status": "pass", "scope": "focused"}
  ],
  "no_residual_gaps": true
}"#,
    )
    .expect("unknown enum record");
    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    assert_eq!(verify.stdout_json()["error"]["code"], "invalid-record-json");
}

#[test]
fn strict_verify_rejects_blank_delta_and_padded_duplicate_identity() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("blank-and-duplicate");
    fs::create_dir_all(&out_dir).expect("raw record dir");
    fs::write(
        out_dir.join("test-first-evidence.json"),
        r#"{
  "schema_version": "test-first-evidence.record.v2",
  "change_classification": "behavior-change",
  "contract_delta": {"changed_behaviors": ["   "]},
  "test_impacts": [
    {
      "target": "tests::contract",
      "disposition": "keep",
      "protected_behavior": "contract",
      "reason": "fixture"
    },
    {
      "target": " tests::contract ",
      "disposition": "keep",
      "protected_behavior": "contract",
      "reason": "fixture"
    }
  ],
  "failing_tests": [{
    "command": "cargo test",
    "exit_code": 101,
    "summary": "red",
    "expected_failure": "missing behavior",
    "observed_failure": "assertion mismatch"
  }],
  "final_validations": [
    {"command": "cargo test", "status": "pass", "scope": "focused"}
  ],
  "no_residual_gaps": true
}"#,
    )
    .expect("raw record");
    let out_arg = out_arg(&out_dir);
    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    let missing = verify.stdout_json()["error"]["details"]["missing"]
        .as_array()
        .expect("missing")
        .clone();
    for expected in [
        "contract_delta_blank_entry",
        "changed_added_or_removed_behavior",
        "duplicate_test_impact",
    ] {
        assert!(missing.iter().any(|item| item == expected), "{expected}");
    }
}

#[test]
fn cli_canonicalizes_duplicate_identities() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("canonical-identities");
    let out_arg = out_arg(&out_dir);
    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
            "--changed-behavior",
            "new behavior",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let first = run(
        tmp.path(),
        &[
            "record-impact",
            "--out",
            &out_arg,
            "--target",
            "tests::contract",
            "--disposition",
            "keep",
            "--protected-behavior",
            "contract",
            "--reason",
            "fixture",
            "--format",
            "json",
        ],
    );
    assert_eq!(first.code, 0);
    let duplicate = run(
        tmp.path(),
        &[
            "record-impact",
            "--out",
            &out_arg,
            "--target",
            " tests::contract ",
            "--disposition",
            "keep",
            "--protected-behavior",
            "contract",
            "--reason",
            "fixture",
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate.code, 65);
    assert_eq!(
        duplicate.stdout_json()["error"]["code"],
        "duplicate-test-impact"
    );

    let first = run(
        tmp.path(),
        &[
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            "cargo test contract",
            "--exit-code",
            "101",
            "--summary",
            "red",
            "--expected-failure",
            "missing behavior",
            "--observed-failure",
            "assertion mismatch",
            "--test-name",
            "contract",
        ],
    );
    assert_eq!(first.code, 0);
    let duplicate = run(
        tmp.path(),
        &[
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            " cargo test contract ",
            "--exit-code",
            "101",
            "--summary",
            "red",
            "--expected-failure",
            "missing behavior",
            "--observed-failure",
            "assertion mismatch",
            "--test-name",
            " contract ",
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate.code, 65);
    assert_eq!(
        duplicate.stdout_json()["error"]["code"],
        "duplicate-failing-evidence"
    );

    let first = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            "cargo test contract",
            "--status",
            "pass",
            "--scope",
            "focused",
            "--format",
            "json",
        ],
    );
    assert_eq!(first.code, 0);
    let duplicate = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            " cargo test contract ",
            "--status",
            "pass",
            "--scope",
            "focused",
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate.code, 65);
    assert_eq!(
        duplicate.stdout_json()["error"]["code"],
        "duplicate-final-validation"
    );

    let first = run(
        tmp.path(),
        &[
            "record-gap",
            "--out",
            &out_arg,
            "--gap",
            "missing browser case",
            "--reason",
            "fixture",
            "--follow-up",
            "issue #1",
            "--format",
            "json",
        ],
    );
    assert_eq!(first.code, 0);
    let duplicate = run(
        tmp.path(),
        &[
            "record-gap",
            "--out",
            &out_arg,
            "--gap",
            " missing browser case ",
            "--reason",
            "fixture",
            "--follow-up",
            "issue #1",
            "--format",
            "json",
        ],
    );
    assert_eq!(duplicate.code, 65);
    assert_eq!(
        duplicate.stdout_json()["error"]["code"],
        "duplicate-residual-gap"
    );
}

#[test]
fn strict_verify_rejects_empty_evidence_identities() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("empty-identities");
    fs::create_dir_all(&out_dir).expect("empty identities dir");
    fs::write(
        out_dir.join("test-first-evidence.json"),
        r#"{
  "schema_version": "test-first-evidence.record.v2",
  "change_classification": "behavior-change",
  "contract_delta": {"changed_behaviors": ["new behavior"]},
  "test_impacts": [{
    "target": "",
    "disposition": "keep",
    "protected_behavior": "contract",
    "reason": "fixture"
  }],
  "failing_tests": [{
    "command": "",
    "exit_code": 101,
    "summary": "",
    "expected_failure": "missing behavior",
    "observed_failure": "assertion mismatch"
  }],
  "final_validations": [
    {"command": "", "status": "pass", "scope": "focused"}
  ],
  "no_residual_gaps": true
}"#,
    )
    .expect("empty identity record");
    let out_arg = out_arg(&out_dir);
    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    let missing = verify.stdout_json()["error"]["details"]["missing"]
        .as_array()
        .expect("missing")
        .clone();
    for expected in [
        "test_impact_identity_and_rationale",
        "meaningful_failing_evidence",
        "final_validation_identity",
    ] {
        assert!(missing.iter().any(|item| item == expected), "{expected}");
    }
}

#[test]
fn final_validation_retry_replaces_failure_but_other_failures_block() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("final-retry");
    let out_arg = out_arg(&out_dir);
    for args in [
        vec![
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
            "--changed-behavior",
            "new behavior",
        ],
        vec![
            "record-impact",
            "--out",
            &out_arg,
            "--none",
            "--reason",
            "no existing owner test",
        ],
        vec![
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            "cargo test contract",
            "--exit-code",
            "101",
            "--summary",
            "red",
            "--expected-failure",
            "new behavior missing",
            "--observed-failure",
            "assertion mismatch",
        ],
        vec!["record-gap", "--out", &out_arg, "--none"],
    ] {
        let output = run(tmp.path(), &args);
        assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    }

    for status in ["fail", "pass"] {
        let output = run(
            tmp.path(),
            &[
                "record-final",
                "--out",
                &out_arg,
                "--command",
                "cargo test contract",
                "--status",
                status,
                "--scope",
                "focused",
            ],
        );
        assert_eq!(
            output.code,
            0,
            "status={status} stderr={}",
            output.stderr_text()
        );
    }
    let verify = run(tmp.path(), &["verify", "--out", &out_arg]);
    assert_eq!(verify.code, 0, "stderr={}", verify.stderr_text());
    let record: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(out_dir.join("test-first-evidence.json")).expect("record"),
    )
    .expect("record json");
    let attempts = record["final_validations"]
        .as_array()
        .expect("final validations");
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["status"], "fail");
    assert_eq!(attempts[0]["attempt"], 1);
    assert_eq!(attempts[1]["status"], "pass");
    assert_eq!(attempts[1]["attempt"], 2);

    let failed_full = run(
        tmp.path(),
        &[
            "record-final",
            "--out",
            &out_arg,
            "--command",
            "cargo test --workspace",
            "--status",
            "fail",
            "--scope",
            "full",
        ],
    );
    assert_eq!(failed_full.code, 0, "stderr={}", failed_full.stderr_text());
    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 1, "stderr={}", verify.stderr_text());
    assert!(
        verify.stdout_json()["error"]["details"]["missing"]
            .as_array()
            .expect("missing")
            .iter()
            .any(|item| item == "failed_final_validation")
    );
}

#[test]
fn secret_like_inputs_are_redacted_in_outputs_and_record() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("secret-safe");
    let out_arg = out_arg(&out_dir);

    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "bug-fix",
            "--changed-behavior",
            "OPENAI_API_KEY=sk-proj-supersecret behavior",
            "--note",
            "OPENAI_API_KEY=sk-proj-supersecret",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let failing = run(
        tmp.path(),
        &[
            "record-failing",
            "--out",
            &out_arg,
            "--command",
            "OPENAI_API_KEY=sk-proj-supersecret cargo test",
            "--exit-code",
            "101",
            "--summary",
            "token: abcdefghijklmnop failure",
            "--expected-failure",
            "secret value is redacted",
            "--observed-failure",
            "OPENAI_API_KEY=sk-proj-supersecret was visible",
            "--format",
            "json",
        ],
    );
    assert_eq!(failing.code, 0, "stderr={}", failing.stderr_text());

    let record = fs::read_to_string(out_dir.join("test-first-evidence.json")).expect("record");
    let combined = format!("{record}\n{}", combined_output(&failing));
    assert!(combined.contains("[REDACTED]"));
    assert!(!combined.contains("sk-proj-supersecret"));
    assert!(!combined.contains("abcdefghijklmnop"));
}

#[test]
fn invalid_maintenance_decisions_fail_with_stable_codes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("invalid-decisions");
    let out_arg = out_arg(&out_dir);
    let init = run(
        tmp.path(),
        &[
            "init",
            "--out",
            &out_arg,
            "--classification",
            "behavior-change",
            "--removed-behavior",
            "removed v1 behavior",
        ],
    );
    assert_eq!(init.code, 0, "stderr={}", init.stderr_text());

    let removal = run(
        tmp.path(),
        &[
            "record-impact",
            "--out",
            &out_arg,
            "--target",
            "tests::v1_behavior",
            "--disposition",
            "remove-obsolete",
            "--protected-behavior",
            "removed v1 behavior",
            "--reason",
            "the behavior is removed",
            "--format",
            "json",
        ],
    );
    assert_eq!(removal.code, 65, "stderr={}", removal.stderr_text());
    assert_eq!(
        removal.stdout_json()["error"]["code"],
        "remove-obsolete-owner-required"
    );

    let deferred = run(
        tmp.path(),
        &[
            "record-waiver",
            "--out",
            &out_arg,
            "--reason",
            "test debt deferred",
            "--waiver-kind",
            "deferred-debt",
            "--why-no-red",
            "harness unavailable",
            "--substitute-validation",
            "manual check",
            "--format",
            "json",
        ],
    );
    assert_eq!(deferred.code, 65, "stderr={}", deferred.stderr_text());
    assert_eq!(
        deferred.stdout_json()["error"]["code"],
        "deferred-waiver-follow-up-required"
    );
}

#[test]
fn missing_record_fails_with_json_error() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("missing");
    let out_arg = out_arg(&out_dir);

    let output = run(tmp.path(), &["show", "--out", &out_arg, "--format", "json"]);

    assert_eq!(output.code, 1, "stderr={}", output.stderr_text());
    let value = output.stdout_json();
    assert_eq!(value["schema_version"], "cli.test-first-evidence.show.v2");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], "missing-record");
}

#[test]
fn legacy_v1_is_readable_but_strict_verify_requires_rerecording() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let out_dir = tmp.path().join("v1-record");
    fs::create_dir_all(&out_dir).expect("v1 record dir");
    fs::write(
        out_dir.join("test-first-evidence.json"),
        r#"{
  "schema_version": "test-first-evidence.record.v1",
  "change_classification": "behavior-change",
  "failing_test": {"command":"cargo test","exit_code":101,"summary":"red","artifacts":[]},
  "final_validation": {"command":"cargo test","status":"pass","artifacts":[]}
}"#,
    )
    .expect("v1 record");
    let out_arg = out_arg(&out_dir);

    let show = run(tmp.path(), &["show", "--out", &out_arg, "--format", "json"]);
    assert_eq!(show.code, 0, "stderr={}", show.stderr_text());
    assert_eq!(
        show.stdout_json()["result"]["record"]["schema_version"],
        "test-first-evidence.record.v1"
    );

    let verify = run(
        tmp.path(),
        &["verify", "--out", &out_arg, "--format", "json"],
    );
    assert_eq!(verify.code, 65, "stderr={}", verify.stderr_text());
    let value = verify.stdout_json();
    assert_eq!(value["error"]["code"], "v1-evidence-record");
    assert!(
        value["error"]["message"]
            .as_str()
            .expect("message")
            .contains("re-record")
    );

    let delivery = run(
        tmp.path(),
        &[
            "check", "--out", &out_arg, "--phase", "delivery", "--format", "json",
        ],
    );
    assert_eq!(delivery.code, 65, "stderr={}", delivery.stderr_text());
    assert_eq!(
        delivery.stdout_json()["error"]["code"],
        "v1-evidence-record"
    );
    assert!(
        delivery.stdout_json()["error"]["message"]
            .as_str()
            .expect("message")
            .contains("re-record")
    );
}

#[test]
fn completion_export_succeeds_outside_git_repo() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["completion", "zsh"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert!(
        output
            .stdout_text()
            .contains("#compdef test-first-evidence"),
        "missing completion header: {}",
        output.stdout_text()
    );
}
