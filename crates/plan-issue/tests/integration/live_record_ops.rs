use std::fs;
use std::path::Path;

use pretty_assertions::{assert_eq, assert_ne};
use serde_json::{Value, json};
use tempfile::TempDir;

use nils_test_support::StubBinDir;
use nils_test_support::cmd::CmdOptions;
use plan_issue::commands::record::RecordProfile;

use crate::common;

const PAYLOAD_SCHEMA_V2: &str = "plan-issue-record.payload.v2";
const PAYLOAD_FENCE_INFO: &str = "plan-issue-record-payload";

/// Build an older `plan-issue-record:v2` comment body with a visible payload
/// fence carrying `data` for the given role/profile.
fn v2_comment_body(role: &str, profile: &str, data: Value) -> String {
    let envelope = json!({
        "schema": PAYLOAD_SCHEMA_V2,
        "role": role,
        "profile": profile,
        "data": data,
    });
    let payload = serde_json::to_string(&envelope).expect("payload json");
    format!(
        "<!-- plan-issue-record:v2 role={role} profile={profile} -->\n\n```{PAYLOAD_FENCE_INFO}\n{payload}\n```\n",
    )
}

fn live_record_options(stub_dir: &Path, envs: &[(&str, &str)]) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
        .with_envs(envs)
}

fn assert_comment_visible_prefix(body: &str, expected: &str) {
    let payload_start = body
        .find("<!-- plan-issue-record-payload:hex:")
        .expect("hidden payload carrier");
    assert_eq!(&body[..payload_start], expected, "{body}");
    assert!(
        body[payload_start..].ends_with(" -->\n"),
        "payload carrier should terminate the comment body:\n{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "payload must remain hidden:\n{body}"
    );
}

fn write_fixture_files(dir: &Path, body: &str, comments: &Value) {
    fs::write(dir.join("issue-body.md"), body).expect("write fixture body");
    fs::write(
        dir.join("comments.json"),
        serde_json::to_string(comments).expect("comments json"),
    )
    .expect("write fixture comments");
}

fn write_pr_fixture(dir: &Path, repo: &str, pr: u64, value: Value) {
    let prs = dir.join("prs");
    fs::create_dir_all(&prs).expect("create prs dir");
    let slug = repo.replace('/', "__");
    fs::write(
        prs.join(format!("{slug}__{pr}.json")),
        serde_json::to_string(&value).expect("pr json"),
    )
    .expect("write pr fixture");
}

fn audit_single_comment_body(body: &str) -> Value {
    let tmp = TempDir::new().expect("tempdir");
    let comments_json = tmp.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": body, "url": "https://github.com/owner/repo/issues/1#issuecomment-record"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_json.to_str().expect("comments path"),
        "--profile",
        "tracking",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    out.stdout_json()["payload"]["result"]["audit"].clone()
}

struct LiveCloseLabelCase<'a> {
    repo: &'a str,
    initial_labels: &'a str,
    repo_labels_json: &'a str,
    drop_label_mutations: bool,
    dry_run: bool,
    explicit_remove: bool,
    local_label_list_unsupported: bool,
    fail_comment: bool,
    automation_label_after_edit: bool,
    fail_close_after_mutation: bool,
    partial_label_edit_once: bool,
}

fn run_live_record_close_label_case_with(
    case: LiveCloseLabelCase<'_>,
) -> (i32, Value, String, String, String) {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let comments_json = comments["comments"].to_string();

    let labels_path = tmp.path().join("labels.txt");
    let issue_state_path = tmp.path().join("issue-state.txt");
    let log_path = tmp.path().join("forge-cli.log");
    let state_dir = tmp.path().join("state-dir");
    fs::write(&labels_path, case.initial_labels).expect("seed labels");
    fs::write(&issue_state_path, "open\n").expect("seed issue state");
    fs::create_dir_all(&state_dir).expect("state dir");

    let labels_s = labels_path.to_string_lossy().to_string();
    let issue_state_s = issue_state_path.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let state_dir_s = state_dir.to_string_lossy().to_string();
    let drop = if case.drop_label_mutations { "1" } else { "0" };
    let local_label_list_unsupported = if case.local_label_list_unsupported {
        "1"
    } else {
        "0"
    };
    let fail_comment = if case.fail_comment { "1" } else { "0" };
    let automation_marker = tmp.path().join("automation-injected");
    let automation_marker_s = automation_marker.to_string_lossy().to_string();
    let automation_label = if case.automation_label_after_edit {
        "automation::complete"
    } else {
        ""
    };
    let fail_close_after_mutation = if case.fail_close_after_mutation {
        "1"
    } else {
        "0"
    };
    let partial_label_edit_marker = tmp.path().join("partial-label-edit");
    let partial_label_edit_marker_s = partial_label_edit_marker.to_string_lossy().to_string();
    let partial_label_edit_once = if case.partial_label_edit_once {
        "1"
    } else {
        "0"
    };
    let strict_repo_labels = if case.repo.starts_with("local:") {
        "0"
    } else {
        "1"
    };
    let mut args = vec!["--format", "json"];
    if case.dry_run {
        args.push("--dry-run");
    }
    args.extend([
        "--repo",
        case.repo,
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--add-label",
        "state::closed",
    ]);
    if case.explicit_remove {
        args.extend(["--remove-label", "state::needs-triage"]);
    }
    let out = common::run_plan_issue_with_options(
        &args,
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LABELS_FILE", &labels_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                ("FORGE_CLI_STUB_DROP_LABEL_MUTATIONS", drop),
                ("FORGE_CLI_STUB_REPO_LABELS_JSON", case.repo_labels_json),
                ("FORGE_CLI_STUB_STRICT_REPO_LABELS", strict_repo_labels),
                (
                    "FORGE_CLI_STUB_LOCAL_LABEL_LIST_UNSUPPORTED",
                    local_label_list_unsupported,
                ),
                ("FORGE_CLI_STUB_FAIL_COMMENT", fail_comment),
                (
                    "FORGE_CLI_STUB_AUTOMATION_LABEL_AFTER_EDIT",
                    automation_label,
                ),
                ("FORGE_CLI_STUB_AUTOMATION_MARKER", &automation_marker_s),
                (
                    "FORGE_CLI_STUB_FAIL_CLOSE_AFTER_MUTATION",
                    fail_close_after_mutation,
                ),
                (
                    "FORGE_CLI_STUB_PARTIAL_LABEL_EDIT_ONCE",
                    partial_label_edit_once,
                ),
                (
                    "FORGE_CLI_STUB_PARTIAL_LABEL_EDIT_MARKER",
                    &partial_label_edit_marker_s,
                ),
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    let envelope = out.stdout_json();
    let log = fs::read_to_string(log_path).expect("provider log");
    let labels = fs::read_to_string(labels_path).expect("provider labels");
    let issue_state = fs::read_to_string(issue_state_path).expect("provider issue state");
    (out.code, envelope, log, labels, issue_state)
}

fn run_live_record_close_label_case(
    drop_label_mutations: bool,
) -> (i32, Value, String, String, String) {
    run_live_record_close_label_case_with(LiveCloseLabelCase {
        repo: "sympoies/agent-runtime-kit",
        initial_labels: "state::needs-triage\n",
        repo_labels_json: r#"[{"name":"state::needs-triage","color":"000000","description":""},{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""}]"#,
        drop_label_mutations,
        dry_run: false,
        explicit_remove: true,
        local_label_list_unsupported: false,
        fail_comment: false,
        automation_label_after_edit: false,
        fail_close_after_mutation: false,
        partial_label_edit_once: false,
    })
}

struct LiveCloseoutCommentCase<'a> {
    comments_json: &'a str,
    comments_after_failed_post_json: Option<&'a str>,
    fail_comment: bool,
    comment_url: &'a str,
}

struct LiveCloseoutCommentResult {
    code: i32,
    envelope: Value,
    log: String,
    captured_comment: Option<String>,
    comment_count: u64,
    issue_state: String,
}

fn run_live_closeout_comment_case(
    scratch: &Path,
    case: LiveCloseoutCommentCase<'_>,
) -> LiveCloseoutCommentResult {
    fs::create_dir_all(scratch).expect("scratch");
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let body_json = json!(body).to_string();
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let labels_path = scratch.join("labels.txt");
    let issue_state_path = scratch.join("issue-state.txt");
    let log_path = scratch.join("forge-cli.log");
    let state_dir = scratch.join("state-dir");
    let capture_path = scratch.join("closeout-comment.md");
    let comment_count_path = scratch.join("comment-count.txt");
    let failed_post_switch = scratch.join("failed-post-switch");
    fs::write(&labels_path, "state::needs-triage\n").expect("seed labels");
    fs::write(&issue_state_path, "open\n").expect("seed issue state");
    fs::create_dir(&state_dir).expect("state dir");
    let labels_s = labels_path.to_string_lossy().to_string();
    let issue_state_s = issue_state_path.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let state_dir_s = state_dir.to_string_lossy().to_string();
    let capture_s = capture_path.to_string_lossy().to_string();
    let comment_count_s = comment_count_path.to_string_lossy().to_string();
    let failed_post_switch_s = failed_post_switch.to_string_lossy().to_string();
    let comments_after = case.comments_after_failed_post_json.unwrap_or("");
    let fail_comment = if case.fail_comment { "1" } else { "0" };

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/agent-runtime-kit",
            "record",
            "close",
            "--issue",
            "42",
            "--linked-pr",
            "sympoies/agent-runtime-kit#1",
            "--approval",
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
            "--add-label",
            "state::closed",
            "--remove-label",
            "state::needs-triage",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", case.comments_json),
                (
                    "FORGE_CLI_STUB_VIEW_COMMENTS_AFTER_SWITCH_JSON",
                    comments_after,
                ),
                (
                    "FORGE_CLI_STUB_VIEW_COMMENTS_SWITCH_FILE",
                    &failed_post_switch_s,
                ),
                (
                    "FORGE_CLI_STUB_FAIL_COMMENT_SWITCH_FILE",
                    &failed_post_switch_s,
                ),
                ("FORGE_CLI_STUB_FAIL_COMMENT", fail_comment),
                ("FORGE_CLI_STUB_COMMENT_URL", case.comment_url),
                ("FORGE_CLI_STUB_CAPTURE_COMMENT_FILE", &capture_s),
                ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &comment_count_s),
                ("FORGE_CLI_STUB_LABELS_FILE", &labels_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    LiveCloseoutCommentResult {
        code: out.code,
        envelope: out.stdout_json(),
        log: fs::read_to_string(log_path).expect("provider log"),
        captured_comment: fs::read_to_string(capture_path).ok(),
        comment_count: fs::read_to_string(comment_count_path)
            .ok()
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0),
        issue_state: fs::read_to_string(issue_state_path).expect("provider issue state"),
    }
}

fn add_live_close_origin(repo: &Path) {
    assert!(
        std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/sympoies/agent-runtime-kit.git",
            ])
            .current_dir(repo)
            .status()
            .expect("git remote add")
            .success()
    );
}

#[derive(Default)]
struct LiveCloseMutation<'a> {
    replace_contents: Option<&'a str>,
    create_lock: bool,
    symlink_target: Option<&'a Path>,
    replace_root: Option<(&'a Path, &'a str)>,
}

fn run_live_record_close_bundle_case_with_mutation(
    repo: &Path,
    bundle: &Path,
    scratch: &Path,
    dry_run: bool,
    fail_close_before_mutation: bool,
    mutation: LiveCloseMutation<'_>,
) -> (i32, Value, String, String) {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let comments_json = comments["comments"].to_string();
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = scratch.join("forge-cli.log");
    let issue_state_path = scratch.join("issue-state.txt");
    let state_dir = scratch.join("state-dir");
    fs::write(&issue_state_path, "open\n").expect("issue state");
    fs::create_dir(&state_dir).expect("state dir");
    let log_s = log_path.to_string_lossy().to_string();
    let issue_state_s = issue_state_path.to_string_lossy().to_string();
    let state_dir_s = state_dir.to_string_lossy().to_string();
    let bundle_s = bundle.to_string_lossy().to_string();
    let fail_close_before_mutation = if fail_close_before_mutation { "1" } else { "0" };
    let replace_on_close_path = bundle
        .join("demo-execution-state.md")
        .to_string_lossy()
        .to_string();
    let replace_on_close_contents = mutation.replace_contents.unwrap_or("");
    let close_lock_wait_path = mutation
        .create_lock
        .then(|| scratch.join("close-lock-acquired"));
    let close_lock_wait_s = close_lock_wait_path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let close_lock_worker = close_lock_wait_path.as_ref().map(|marker| {
        let issue_state = issue_state_path.clone();
        let lock_path = std::path::PathBuf::from(format!("{replace_on_close_path}.lock"));
        let marker = marker.clone();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline
                && fs::read_to_string(&issue_state).ok().as_deref() != Some("closed\n")
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(
                fs::read_to_string(&issue_state).expect("provider issue state"),
                "closed\n"
            );
            let _active_lock = plan_tooling::mutation_lock::OwnedFileLock::acquire(&lock_path)
                .expect("hold apply-time advisory lock");
            fs::write(&marker, "acquired\n").expect("signal advisory lock acquisition");
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("release apply-time advisory lock");
        });
        (release_tx, handle)
    });
    let symlink_on_close_path = mutation
        .symlink_target
        .map(|_| replace_on_close_path.clone())
        .unwrap_or_default();
    let symlink_on_close_target = mutation
        .symlink_target
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let rename_root_on_close_path = mutation
        .replace_root
        .map(|_| repo.to_string_lossy().to_string())
        .unwrap_or_default();
    let rename_root_on_close_target = mutation
        .replace_root
        .map(|(path, _)| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let rename_root_state_relative = mutation
        .replace_root
        .map(|_| {
            bundle
                .join("demo-execution-state.md")
                .strip_prefix(repo)
                .expect("execution state relative to repository")
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default();
    let rename_root_state_contents = mutation
        .replace_root
        .map(|(_, contents)| contents)
        .unwrap_or_default();

    let mut args = vec!["--format", "json"];
    if dry_run {
        args.push("--dry-run");
    }
    args.extend([
        "--repo",
        "https://github.com/sympoies/agent-runtime-kit",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--bundle",
        &bundle_s,
    ]);
    let out = common::run_plan_issue_with_options(
        &args,
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                (
                    "FORGE_CLI_STUB_FAIL_CLOSE_BEFORE_MUTATION",
                    fail_close_before_mutation,
                ),
                (
                    "FORGE_CLI_STUB_REPLACE_ON_CLOSE_PATH",
                    &replace_on_close_path,
                ),
                (
                    "FORGE_CLI_STUB_REPLACE_ON_CLOSE_CONTENTS",
                    replace_on_close_contents,
                ),
                ("FORGE_CLI_STUB_WAIT_ON_CLOSE_PATH", &close_lock_wait_s),
                (
                    "FORGE_CLI_STUB_SYMLINK_ON_CLOSE_PATH",
                    &symlink_on_close_path,
                ),
                (
                    "FORGE_CLI_STUB_SYMLINK_ON_CLOSE_TARGET",
                    &symlink_on_close_target,
                ),
                (
                    "FORGE_CLI_STUB_RENAME_ROOT_ON_CLOSE_PATH",
                    &rename_root_on_close_path,
                ),
                (
                    "FORGE_CLI_STUB_RENAME_ROOT_ON_CLOSE_TARGET",
                    &rename_root_on_close_target,
                ),
                (
                    "FORGE_CLI_STUB_RENAME_ROOT_STATE_RELATIVE",
                    &rename_root_state_relative,
                ),
                (
                    "FORGE_CLI_STUB_RENAME_ROOT_STATE_CONTENTS",
                    rename_root_state_contents,
                ),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        )
        .with_cwd(repo),
    );
    if let Some((release_tx, handle)) = close_lock_worker {
        release_tx.send(()).expect("release lock worker");
        handle.join().expect("join lock worker");
    }

    (
        out.code,
        out.stdout_json(),
        fs::read_to_string(issue_state_path).expect("issue state"),
        fs::read_to_string(log_path).expect("provider log"),
    )
}

fn run_live_record_close_bundle_case(
    repo: &Path,
    bundle: &Path,
    scratch: &Path,
    dry_run: bool,
    fail_close_before_mutation: bool,
    replace_on_close_contents: Option<&str>,
) -> (i32, Value, String, String) {
    run_live_record_close_bundle_case_with_mutation(
        repo,
        bundle,
        scratch,
        dry_run,
        fail_close_before_mutation,
        LiveCloseMutation {
            replace_contents: replace_on_close_contents,
            ..Default::default()
        },
    )
}

const LIVE_CLOSE_EXECUTION_STATE: &str = "## Execution State\n\n- Status: in-progress\n- Current task: close tracker\n- Next task: none\n- Last updated: 2026-07-18\n- Branch/commit/PR: pending\n- Tracking issue: not yet opened\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 1.1 | done | demo | test |\n\n## Handoff\n\n- Close the tracker.\n";

#[test]
fn record_close_uses_tracking_authority_for_linked_prs() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let comments_json = comments["comments"].to_string();
    let cases = [
        (
            "bare ref inherits tracker authority",
            "team/service#7",
            "gitlab.corp.example",
        ),
        (
            "qualified MR matches tracker authority",
            "https://gitlab.corp.example/team/service/-/merge_requests/7",
            "gitlab.corp.example",
        ),
    ];

    for (name, linked_pr, expected_host) in cases {
        let tmp = TempDir::new().expect("tempdir");
        let stub = StubBinDir::new();
        stub.write_exe("forge-cli", common::forge_cli_stub_script());
        let log_path = tmp.path().join("forge-cli.log");
        let log_s = log_path.to_string_lossy().to_string();
        let out = common::run_plan_issue_with_options(
            &[
                "--format",
                "json",
                "--dry-run",
                "--repo",
                "https://gitlab.corp.example/acme/widgets",
                "record",
                "close",
                "--issue",
                "42",
                "--linked-pr",
                linked_pr,
                "--approval",
                "https://gitlab.corp.example/acme/widgets/-/issues/42#note_approval",
            ],
            live_record_options(
                stub.path(),
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                    ("FORGE_CLI_STUB_LOG", &log_s),
                ],
            ),
        );
        assert_eq!(
            out.code,
            0,
            "{name}: stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
        let log = fs::read_to_string(&log_path).expect("provider log");
        let pr_lines = log
            .lines()
            .filter(|line| line.contains("pr view 7") || line.contains("pr checks 7"))
            .collect::<Vec<_>>();
        assert!(!pr_lines.is_empty(), "{name}: {log}");
        assert!(
            pr_lines
                .iter()
                .all(|line| line.contains(&format!("--host {expected_host}"))),
            "{name}: linked PR authority drifted: {log}"
        );
        assert!(
            pr_lines
                .iter()
                .all(|line| line.contains("--repo team/service")),
            "{name}: linked PR slug drifted: {log}"
        );
    }
}

#[test]
fn record_close_rejects_linked_pr_on_different_authority_before_provider_access() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let comments_json = comments["comments"].to_string();

    for linked_pr in [
        "https://gitlab.other.example/team/service/-/merge_requests/7",
        "https://github.enterprise.example/team/service/pull/7",
    ] {
        let tmp = TempDir::new().expect("tempdir");
        let stub = StubBinDir::new();
        stub.write_exe("forge-cli", common::forge_cli_stub_script());
        let log_path = tmp.path().join("forge-cli.log");
        let log_s = log_path.to_string_lossy().to_string();
        let out = common::run_plan_issue_with_options(
            &[
                "--format",
                "json",
                "--dry-run",
                "--repo",
                "https://gitlab.corp.example/acme/widgets",
                "record",
                "close",
                "--issue",
                "42",
                "--linked-pr",
                linked_pr,
                "--approval",
                "approved",
            ],
            live_record_options(
                stub.path(),
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                    ("FORGE_CLI_STUB_LOG", &log_s),
                ],
            ),
        );

        assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
        assert_eq!(
            out.stdout_json()["error"]["code"],
            "record-linked-pr-authority-mismatch"
        );
        let log = fs::read_to_string(log_path).expect("provider log");
        assert!(!log.contains("pr view 7"), "{log}");
        assert!(!log.contains("pr checks 7"), "{log}");
    }
}

#[test]
fn record_close_rejects_cross_forge_url_authority_conflict() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let comments_json = comments["comments"].to_string();
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--dry-run",
            "--repo",
            "https://gitlab.corp.example/acme/widgets",
            "record",
            "close",
            "--issue",
            "42",
            "--linked-pr",
            "https://github.com/team/service/-/merge_requests/7",
            "--approval",
            "approved",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );

    assert_ne!(out.code, 0, "stdout: {}", out.stdout_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "record-linked-pr-authority-ambiguous"
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    assert!(!log.contains("pr view 7"), "{log}");
    assert!(!log.contains("pr checks 7"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue edit 42"), "{log}");
}

#[test]
fn record_close_uses_final_refresh_evidence_for_terminal_projection() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let mut comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let initial_comments = comments["comments"].to_string();
    let final_validation_url =
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-validation-final";
    comments["comments"]
        .as_array_mut()
        .expect("comments array")
        .push(json!({
            "url": final_validation_url,
            "created_at": "2099-01-01T00:00:00Z",
            "body": v2_comment_body(
                "validation",
                "tracking",
                json!({
                    "overall": "pass",
                    "commands": [{"command": "cargo test --final", "status": "pass"}],
                    "waivers": []
                }),
            ),
        }));
    let refreshed_comments = comments["comments"].to_string();
    let body_json = json!(body).to_string();
    let evidence_marker = tmp.path().join("evidence-marker");
    let pr_marker = tmp.path().join("pr-marker");
    let capture_comment = tmp.path().join("closeout.md");
    let capture_dashboard = tmp.path().join("dashboard.md");
    let issue_state = tmp.path().join("issue-state.txt");
    let state_dir = tmp.path().join("state-dir");
    let log_path = tmp.path().join("forge-cli.log");
    fs::write(&issue_state, "open\n").expect("issue state");
    fs::create_dir(&state_dir).expect("state dir");
    let evidence_marker_s = evidence_marker.to_string_lossy().to_string();
    let pr_marker_s = pr_marker.to_string_lossy().to_string();
    let capture_comment_s = capture_comment.to_string_lossy().to_string();
    let capture_dashboard_s = capture_dashboard.to_string_lossy().to_string();
    let issue_state_s = issue_state.to_string_lossy().to_string();
    let state_dir_s = state_dir.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://github.com/sympoies/agent-runtime-kit",
            "record",
            "close",
            "--issue",
            "42",
            "--linked-pr",
            "sympoies/agent-runtime-kit#1",
            "--approval",
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &initial_comments),
                (
                    "FORGE_CLI_STUB_VIEW_COMMENTS_AFTER_FIRST_JSON",
                    &refreshed_comments,
                ),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_MARKER", &evidence_marker_s),
                ("FORGE_CLI_STUB_MERGE_SHA", "initial-sha"),
                ("FORGE_CLI_STUB_MERGE_SHA_AFTER_FIRST", "final-sha"),
                ("FORGE_CLI_STUB_MERGE_SHA_MARKER", &pr_marker_s),
                ("FORGE_CLI_STUB_CAPTURE_COMMENT_FILE", &capture_comment_s),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &capture_dashboard_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("PLAN_ISSUE_HOME", &state_dir_s),
            ],
        ),
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let result = &out.stdout_json()["payload"]["result"];
    assert_eq!(result["linked_prs"][0]["merge_sha"], "final-sha");
    assert!(
        result["final_dashboard"]
            .as_str()
            .expect("final dashboard")
            .contains(final_validation_url),
        "{}",
        result["final_dashboard"]
    );
    let closeout_body = fs::read_to_string(&capture_comment).expect("captured closeout");
    let closeout_audit = audit_single_comment_body(&closeout_body);
    let closeout = &closeout_audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_validation_url"], final_validation_url);
    assert_eq!(closeout["linked_prs"][0]["merge_sha"], "final-sha");

    let log = fs::read_to_string(log_path).expect("provider log");
    let mutations = log
        .lines()
        .filter(|line| {
            line.contains("issue close 42")
                || line.contains("issue comment 42")
                || line.contains("issue edit 42")
        })
        .collect::<Vec<_>>();
    assert!(mutations[0].contains("issue close 42"), "{log}");
}

#[test]
fn record_close_rechecks_changed_gate_before_provider_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let mut comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let original_comments = comments["comments"].to_string();
    comments["comments"]
        .as_array_mut()
        .expect("comments array")
        .push(json!({
            "url": "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-validation-failed",
            "created_at": "2026-05-22T19:00:00Z",
            "body": v2_comment_body(
                "validation",
                "tracking",
                json!({
                    "overall": "fail",
                    "commands": [{"command": "cargo test", "status": "fail"}],
                    "waivers": []
                }),
            ),
        }));
    let changed_comments = comments["comments"].to_string();
    let marker = tmp.path().join("first-evidence-read");
    let issue_state = tmp.path().join("issue-state.txt");
    let log_path = tmp.path().join("forge-cli.log");
    fs::write(&issue_state, "open\n").expect("issue state");
    let body_json = json!(body).to_string();
    let marker_s = marker.to_string_lossy().to_string();
    let issue_state_s = issue_state.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://github.com/sympoies/agent-runtime-kit",
            "record",
            "close",
            "--issue",
            "42",
            "--linked-pr",
            "sympoies/agent-runtime-kit#1",
            "--approval",
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &original_comments),
                (
                    "FORGE_CLI_STUB_VIEW_COMMENTS_AFTER_FIRST_JSON",
                    &changed_comments,
                ),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_MARKER", &marker_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );

    assert_eq!(out.code, 1, "stdout={}", out.stdout_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "record-close-gate-changed"
    );
    assert_eq!(
        fs::read_to_string(issue_state).expect("issue state"),
        "open\n"
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    assert_eq!(
        log.matches("issue view 42 --with-comments").count(),
        2,
        "{log}"
    );
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_rechecks_linked_pr_gate_before_provider_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let comments_json = comments["comments"].to_string();
    let marker = tmp.path().join("first-pr-check-read");
    let issue_state = tmp.path().join("issue-state.txt");
    let log_path = tmp.path().join("forge-cli.log");
    fs::write(&issue_state, "open\n").expect("issue state");
    let body_json = json!(body).to_string();
    let marker_s = marker.to_string_lossy().to_string();
    let issue_state_s = issue_state.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://github.com/sympoies/agent-runtime-kit",
            "record",
            "close",
            "--issue",
            "42",
            "--linked-pr",
            "sympoies/agent-runtime-kit#1",
            "--approval",
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_REQUIRED_COUNT", "1"),
                ("FORGE_CLI_STUB_CHECKS_STATE_AFTER_FIRST", "failure"),
                ("FORGE_CLI_STUB_CHECKS_STATE_MARKER", &marker_s),
                ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                ("FORGE_CLI_STUB_LOG", &log_s),
            ],
        ),
    );

    assert_eq!(out.code, 1, "stdout={}", out.stdout_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "record-close-gate-changed"
    );
    assert_eq!(
        fs::read_to_string(issue_state).expect("issue state"),
        "open\n"
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    assert_eq!(
        log.matches("pr checks 1 --required-only").count(),
        2,
        "{log}"
    );
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_rejects_outside_bundle_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let outside_bundle = tmp.path().join("outside-bundle");
    fs::create_dir(&outside_bundle).expect("outside bundle");
    fs::write(
        outside_bundle.join("sample-execution-state.md"),
        LIVE_CLOSE_EXECUTION_STATE,
    )
    .expect("execution state");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case(
        repo.path(),
        &outside_bundle,
        tmp.path(),
        false,
        false,
        None,
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-path-invalid"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_provider_dry_run_preflights_outside_bundle() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let outside_bundle = tmp.path().join("outside-bundle");
    fs::create_dir(&outside_bundle).expect("outside bundle");
    fs::write(
        outside_bundle.join("sample-execution-state.md"),
        LIVE_CLOSE_EXECUTION_STATE,
    )
    .expect("execution state");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case(
        repo.path(),
        &outside_bundle,
        tmp.path(),
        true,
        false,
        None,
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-path-invalid"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_provider_dry_run_leaves_bundle_directory_unchanged() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    fs::write(
        bundle.join("demo-execution-state.md"),
        LIVE_CLOSE_EXECUTION_STATE,
    )
    .expect("execution state");
    let entries_before = fs::read_dir(&bundle)
        .expect("bundle entries")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    let status_before = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .expect("git status before")
        .stdout;

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), true, false, None);

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
    let entries_after = fs::read_dir(&bundle)
        .expect("bundle entries after")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        entries_after, entries_before,
        "dry run created a bundle entry"
    );
    let status_after = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo.path())
        .output()
        .expect("git status after")
        .stdout;
    assert_eq!(status_after, status_before, "dry run changed Git status");
}

#[test]
fn record_close_rejects_malformed_execution_state_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    fs::write(
        bundle.join("demo-execution-state.md"),
        "not an execution state\n",
    )
    .expect("malformed execution state");

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, false, None);

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-ledger-malformed"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_rejects_nonterminal_malformed_and_ambiguous_ledgers_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let cases = [
        (
            "nonterminal",
            LIVE_CLOSE_EXECUTION_STATE.replace(
                "| 1.1 | done | demo | test |",
                "| 1.1 | pending | demo | test |",
            ),
            "record-close-execution-state-ledger-pending",
        ),
        (
            "malformed",
            LIVE_CLOSE_EXECUTION_STATE
                .replace("| 1.1 | done | demo | test |", "1.1 | done | demo | test |"),
            "record-close-execution-state-ledger-malformed",
        ),
        (
            "ambiguous",
            LIVE_CLOSE_EXECUTION_STATE.replace(
                "| 1.1 | done | demo | test |",
                "| 1.1 | done | demo | test |\n| 1.1 | waived | duplicate | waiver |",
            ),
            "record-close-execution-state-ledger-ambiguous",
        ),
    ];

    for (name, contents, expected_code) in cases {
        let tmp = TempDir::new().expect("tempdir");
        let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
        add_live_close_origin(repo.path());
        let bundle = repo.path().join("docs/plans/demo");
        fs::create_dir_all(&bundle).expect("bundle");
        let execution_state = bundle.join("demo-execution-state.md");
        fs::write(&execution_state, &contents).expect("execution state");
        let before = fs::read(&execution_state).expect("execution state before");

        let (code, envelope, issue_state, log) =
            run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, false, None);

        assert_ne!(code, 0, "{name}: {envelope}");
        assert_eq!(envelope["error"]["code"], expected_code, "{name}");
        assert_eq!(issue_state, "open\n", "{name}");
        assert!(!log.contains("issue edit 42"), "{name}: {log}");
        assert!(!log.contains("issue comment 42"), "{name}: {log}");
        assert!(!log.contains("issue close 42"), "{name}: {log}");
        assert_eq!(
            fs::read(&execution_state).expect("execution state after"),
            before,
            "{name}: rejected ledger must not receive terminal headers"
        );
    }
}

#[test]
fn record_close_rejects_ambiguous_execution_state_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    for name in ["a-execution-state.md", "b-execution-state.md"] {
        fs::write(bundle.join(name), LIVE_CLOSE_EXECUTION_STATE).expect("execution state");
    }

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, false, None);

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-ambiguous"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[cfg(unix)]
#[test]
fn record_close_rejects_symlinked_execution_state_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let target = bundle.join("real-execution-state.md");
    fs::write(&target, LIVE_CLOSE_EXECUTION_STATE).expect("execution state target");
    symlink(&target, bundle.join("demo-execution-state.md")).expect("execution state symlink");

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, false, None);

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-path-invalid"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[cfg(unix)]
#[test]
fn record_close_rejects_symlinked_bundle_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let real_bundle = repo.path().join("docs/plans/real-demo");
    fs::create_dir_all(&real_bundle).expect("bundle");
    fs::write(
        real_bundle.join("demo-execution-state.md"),
        LIVE_CLOSE_EXECUTION_STATE,
    )
    .expect("execution state");
    let linked_bundle = repo.path().join("docs/plans/demo");
    symlink(&real_bundle, &linked_bundle).expect("bundle symlink");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case(
        repo.path(),
        &linked_bundle,
        tmp.path(),
        false,
        false,
        None,
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-path-invalid"
    );
    assert_eq!(issue_state, "open\n");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue close 42"), "{log}");
}

#[test]
fn record_close_provider_failure_does_not_write_execution_state() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");
    let before = fs::read(&execution_state).expect("execution state before");

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, true, None);

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(envelope["error"]["code"], "record-close-issue-close-failed");
    assert_eq!(issue_state, "open\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
    assert!(!log.contains("issue edit 42"), "{log}");
    assert_eq!(
        fs::read(execution_state).expect("execution state after"),
        before,
        "execution-state writeback must wait for provider closure"
    );
}

#[test]
fn record_close_rejects_sensitive_linked_pr_urls_without_reflecting_secrets() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    for linked_pr in [
        "https://operator:secret-token@github.com/sympoies/agent-runtime-kit/pull/1",
        "https://github.com/sympoies/agent-runtime-kit/pull/1?token=secret-token",
        "https://github.com/sympoies/agent-runtime-kit/pull/1#secret-token",
    ] {
        let out = common::run_plan_issue_local(&[
            "--format",
            "json",
            "record",
            "close",
            "--fixture",
            fixture.to_str().expect("fixture"),
            "--issue",
            "42",
            "--linked-pr",
            linked_pr,
            "--approval",
            "approved",
        ]);

        assert_eq!(
            out.code,
            64,
            "stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
        assert_eq!(
            out.stdout_json()["error"]["code"],
            "record-invalid-linked-pr"
        );
        assert!(!out.stdout_text().contains("operator"));
        assert!(!out.stdout_text().contains("secret-token"));
        assert!(!out.stderr_text().contains("operator"));
        assert!(!out.stderr_text().contains("secret-token"));
    }
}

#[test]
fn record_close_does_not_terminalize_execution_state_replaced_during_provider_close() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    const REPLACEMENT: &str = "## Execution State\n\n- Status: blocked\n- Current task: replacement\n- Next task: investigate\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 9.9 | blocked | replacement state | none |\n";

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case(
        repo.path(),
        &bundle,
        tmp.path(),
        false,
        false,
        Some(REPLACEMENT),
    );

    assert_eq!(code, 1, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-writeback-failed"
    );
    let message = envelope["error"]["message"].as_str().expect("message");
    assert!(message.contains("provider issue is closed"), "{message}");
    assert!(
        message.contains("contents changed after preflight"),
        "{message}"
    );
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert_eq!(
        fs::read_to_string(execution_state).expect("replacement after close"),
        REPLACEMENT,
        "a replacement file must not receive the preflighted terminal patch"
    );
}

#[test]
fn record_close_reports_busy_apply_lock_after_provider_close() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case_with_mutation(
        repo.path(),
        &bundle,
        tmp.path(),
        false,
        false,
        LiveCloseMutation {
            create_lock: true,
            ..Default::default()
        },
    );

    assert_eq!(code, 1, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-writeback-failed"
    );
    let message = envelope["error"]["message"].as_str().expect("message");
    assert!(message.contains("provider issue is closed"), "{message}");
    assert!(
        message.contains("exec-state-mutation-lock-busy"),
        "{message}"
    );
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert_eq!(
        fs::read_to_string(execution_state).expect("execution state after close"),
        LIVE_CLOSE_EXECUTION_STATE
    );
}

#[cfg(unix)]
#[test]
fn record_close_rejects_apply_time_symlink_after_provider_close() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    const TARGET: &str = "outside replacement must remain unchanged\n";

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");
    let target = tmp.path().join("outside.md");
    fs::write(&target, TARGET).expect("symlink target");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case_with_mutation(
        repo.path(),
        &bundle,
        tmp.path(),
        false,
        false,
        LiveCloseMutation {
            symlink_target: Some(&target),
            ..Default::default()
        },
    );

    assert_eq!(code, 1, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-writeback-failed"
    );
    let message = envelope["error"]["message"].as_str().expect("message");
    assert!(message.contains("provider issue is closed"), "{message}");
    assert!(
        message.contains("path changed after preflight"),
        "{message}"
    );
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert_eq!(fs::read_to_string(target).expect("symlink target"), TARGET);
    assert!(
        fs::symlink_metadata(execution_state)
            .expect("replacement symlink")
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn record_close_rejects_repository_root_replacement_after_provider_close() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    const REPLACEMENT: &str = "## Execution State\n\n- Status: blocked\n- Current task: replacement root\n- Next task: investigate\n\n## Task Ledger\n\n| ID | Status | Task | Evidence |\n| --- | --- | --- | --- |\n| 9.9 | blocked | replacement root state | none |\n";

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let repo_path = repo.path().to_path_buf();
    let displaced = repo_path.parent().expect("repository parent").join(format!(
        "{}-displaced",
        repo_path
            .file_name()
            .expect("repository directory name")
            .to_string_lossy()
    ));
    let bundle = repo_path.join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");

    let (code, envelope, issue_state, log) = run_live_record_close_bundle_case_with_mutation(
        &repo_path,
        &bundle,
        tmp.path(),
        false,
        false,
        LiveCloseMutation {
            replace_root: Some((&displaced, REPLACEMENT)),
            ..Default::default()
        },
    );

    let displaced_state = displaced.join("docs/plans/demo/demo-execution-state.md");
    let displaced_contents =
        fs::read_to_string(&displaced_state).expect("displaced execution state");
    let replacement_contents =
        fs::read_to_string(&execution_state).expect("replacement execution state");
    fs::remove_dir_all(&repo_path).expect("remove replacement repository root");
    fs::rename(&displaced, &repo_path).expect("restore original repository root");

    assert_eq!(code, 1, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-execution-state-writeback-failed"
    );
    let message = envelope["error"]["message"].as_str().expect("message");
    assert!(message.contains("provider issue is closed"), "{message}");
    assert!(
        message.contains("path changed after preflight"),
        "{message}"
    );
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert_eq!(displaced_contents, LIVE_CLOSE_EXECUTION_STATE);
    assert_eq!(replacement_contents, REPLACEMENT);
}

#[test]
fn record_close_dashboard_failure_precedes_execution_state_writeback_and_retry_reuses_closeout() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    const CLOSEOUT_URL: &str =
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-closeout";

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");
    let before = fs::read(&execution_state).expect("execution state before");

    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let body = fs::read_to_string(fixture.join("issue-body.md")).expect("fixture body");
    let mut comments: Value = serde_json::from_str(
        &fs::read_to_string(fixture.join("comments.json")).expect("fixture comments"),
    )
    .expect("comments json");
    let body_json = json!(body).to_string();
    let initial_comments = comments["comments"].to_string();
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let issue_state = tmp.path().join("issue-state.txt");
    let state_dir = tmp.path().join("state-dir");
    let log_path = tmp.path().join("forge-cli.log");
    let closeout_capture = tmp.path().join("closeout.md");
    let dashboard_capture = tmp.path().join("dashboard.md");
    let comment_count = tmp.path().join("comment-count.txt");
    let body_edit_failure = tmp.path().join("body-edit-failed");
    fs::write(&issue_state, "open\n").expect("issue state");
    fs::create_dir(&state_dir).expect("state dir");
    let issue_state_s = issue_state.to_string_lossy().to_string();
    let state_dir_s = state_dir.to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let closeout_capture_s = closeout_capture.to_string_lossy().to_string();
    let dashboard_capture_s = dashboard_capture.to_string_lossy().to_string();
    let comment_count_s = comment_count.to_string_lossy().to_string();
    let body_edit_failure_s = body_edit_failure.to_string_lossy().to_string();
    let bundle_s = bundle.to_string_lossy().to_string();
    let args = [
        "--format",
        "json",
        "--repo",
        "https://github.com/sympoies/agent-runtime-kit",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--bundle",
        &bundle_s,
    ];
    let run = |comments_json: &str| {
        common::run_plan_issue_with_options(
            &args,
            live_record_options(
                stub.path(),
                &[
                    ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                    ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", comments_json),
                    ("FORGE_CLI_STUB_ISSUE_STATE_FILE", &issue_state_s),
                    ("FORGE_CLI_STUB_CAPTURE_COMMENT_FILE", &closeout_capture_s),
                    ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &dashboard_capture_s),
                    ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &comment_count_s),
                    ("FORGE_CLI_STUB_COMMENT_URL", CLOSEOUT_URL),
                    (
                        "FORGE_CLI_STUB_FAIL_BODY_EDIT_ONCE_MARKER",
                        &body_edit_failure_s,
                    ),
                    ("FORGE_CLI_STUB_LOG", &log_s),
                    ("PLAN_ISSUE_HOME", &state_dir_s),
                ],
            )
            .with_cwd(repo.path()),
        )
    };

    let first = run(&initial_comments);
    assert_eq!(first.code, 1, "stdout={}", first.stdout_text());
    assert_eq!(
        first.stdout_json()["error"]["code"],
        "record-close-dashboard-edit-failed"
    );
    assert_eq!(
        fs::read(&execution_state).expect("execution state after dashboard failure"),
        before,
        "local terminal writeback must wait for final dashboard repair"
    );
    assert_eq!(
        fs::read_to_string(&issue_state).expect("provider issue state"),
        "closed\n"
    );
    assert_eq!(
        fs::read_to_string(&comment_count).expect("comment count"),
        "1\n"
    );
    let first_log = fs::read_to_string(&log_path).expect("provider log");
    let close = first_log.find("issue close 42").expect("close mutation");
    let closeout = first_log
        .find("issue comment 42")
        .expect("closeout mutation");
    let dashboard = first_log.find("issue edit 42").expect("dashboard mutation");
    assert!(close < closeout && closeout < dashboard, "{first_log}");

    let closeout_body = fs::read_to_string(&closeout_capture).expect("closeout body");
    comments["comments"]
        .as_array_mut()
        .expect("comments array")
        .push(json!({
            "body": closeout_body,
            "url": CLOSEOUT_URL,
            "created_at": "2099-01-01T00:00:00Z"
        }));
    let retry_comments = comments["comments"].to_string();
    let retry = run(&retry_comments);
    assert_eq!(
        retry.code,
        0,
        "stdout={} stderr={}",
        retry.stdout_text(),
        retry.stderr_text()
    );
    assert_eq!(
        fs::read_to_string(&comment_count).expect("comment count after retry"),
        "1\n",
        "retry must reuse the semantically matching closeout"
    );
    let written = fs::read_to_string(&execution_state).expect("terminal execution state");
    assert!(written.contains("- Status: complete\n"), "{written}");
    assert!(written.contains("- Current task: complete\n"), "{written}");
    assert!(written.contains("- Next task: none\n"), "{written}");
    let dashboard = fs::read_to_string(&dashboard_capture).expect("final dashboard");
    assert!(
        dashboard.starts_with("<!-- plan-issue-record-identity:v1:hex:")
            && dashboard.contains("\n\n## Final Dashboard"),
        "{dashboard}"
    );
}

#[test]
fn record_close_writes_confined_execution_state_after_provider_close() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    add_live_close_origin(repo.path());
    let bundle = repo.path().join("docs/plans/demo");
    fs::create_dir_all(&bundle).expect("bundle");
    let execution_state = bundle.join("demo-execution-state.md");
    fs::write(&execution_state, LIVE_CLOSE_EXECUTION_STATE).expect("execution state");

    let (code, envelope, issue_state, log) =
        run_live_record_close_bundle_case(repo.path(), &bundle, tmp.path(), false, false, None);

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert_eq!(
        envelope["payload"]["result"]["execution_state_sync"]["changed"],
        true
    );
    let written = fs::read_to_string(execution_state).expect("written execution state");
    assert!(written.contains("- Status: complete\n"));
    assert!(written.contains("- Current task: complete\n"));
    assert!(written.contains("- Next task: none\n"));
}

#[test]
fn record_close_reports_partial_label_edit_after_close_without_rollback() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: true,
        },
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(envelope["error"]["code"], "record-close-label-edit-failed");
    assert_eq!(labels, "state::ready\nstate::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert_eq!(log.matches("--add-label").count(), 1, "{log}");
    assert!(!log.contains("--remove-label state::closed"), "{log}");
    assert!(!log.contains("issue comment 42"), "{log}");
}

#[test]
fn record_close_preserves_terminal_and_automation_labels_when_comment_fails() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""},{"name":"automation::complete","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: true,
            automation_label_after_edit: true,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        },
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-comment-post-failed"
    );
    assert_eq!(labels, "state::closed\nautomation::complete\n");
    assert_eq!(issue_state, "closed\n");
    assert_eq!(log.matches("issue edit 42").count(), 1, "{log}");
    assert!(
        !log.contains("--remove-label automation::complete"),
        "{log}"
    );
}

#[test]
fn record_close_live_reuses_and_recovers_latest_semantic_closeout_without_repost() {
    const EXISTING_URL: &str =
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-existing-closeout";

    let tmp = TempDir::new().expect("tempdir");
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout/comments.json");
    let fixture_comments: Value =
        serde_json::from_str(&fs::read_to_string(fixture).expect("fixture comments"))
            .expect("comments json");
    let initial_comments = fixture_comments["comments"].to_string();

    let seed = run_live_closeout_comment_case(
        &tmp.path().join("seed"),
        LiveCloseoutCommentCase {
            comments_json: &initial_comments,
            comments_after_failed_post_json: None,
            fail_comment: false,
            comment_url: EXISTING_URL,
        },
    );
    assert_eq!(seed.code, 0, "{}", seed.envelope);
    assert_eq!(seed.comment_count, 1, "{}", seed.log);
    assert_eq!(seed.issue_state, "closed\n");
    let closeout = seed.captured_comment.expect("captured closeout comment");
    let carrier_prefix = "<!-- plan-issue-record-payload:hex:";
    let carrier_start =
        closeout.find(carrier_prefix).expect("payload carrier") + carrier_prefix.len();
    let carrier_end = carrier_start
        + closeout[carrier_start..]
            .find(" -->")
            .expect("payload carrier end");
    let hex = &closeout[carrier_start..carrier_end];
    assert_eq!(hex.len() % 2, 0, "payload carrier hex width");
    let payload_bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("hex pair"), 16)
                .expect("payload hex")
        })
        .collect::<Vec<_>>();
    let mut payload: Value = serde_json::from_slice(&payload_bytes).expect("closeout payload");
    payload["updated_at"] = json!("2000-01-01T00:00:00Z");
    let stale_hex = serde_json::to_vec(&payload)
        .expect("render closeout payload")
        .into_iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let stale_timestamp_closeout = format!(
        "{}{}{}",
        &closeout[..carrier_start],
        stale_hex,
        &closeout[carrier_end..]
    );
    let mut comments = fixture_comments["comments"]
        .as_array()
        .expect("fixture comments array")
        .clone();
    comments.push(json!({
        "url": EXISTING_URL,
        "created_at": "2030-01-01T00:00:00Z",
        "body": stale_timestamp_closeout,
    }));
    let comments_with_closeout = serde_json::to_string(&comments).expect("comments with closeout");

    let retry = run_live_closeout_comment_case(
        &tmp.path().join("retry"),
        LiveCloseoutCommentCase {
            comments_json: &comments_with_closeout,
            comments_after_failed_post_json: None,
            fail_comment: false,
            comment_url: "https://github.com/sympoies/agent-runtime-kit/issues/42#unexpected-repost",
        },
    );
    assert_eq!(retry.code, 0, "{}", retry.envelope);
    assert_eq!(retry.comment_count, 0, "{}", retry.log);
    assert!(!retry.log.contains("issue comment 42"), "{}", retry.log);
    assert_eq!(
        retry.envelope["payload"]["result"]["closeout_url"],
        EXISTING_URL
    );
    assert_eq!(retry.issue_state, "closed\n");

    let recovered = run_live_closeout_comment_case(
        &tmp.path().join("recover"),
        LiveCloseoutCommentCase {
            comments_json: &initial_comments,
            comments_after_failed_post_json: Some(&comments_with_closeout),
            fail_comment: true,
            comment_url: "https://github.com/sympoies/agent-runtime-kit/issues/42#ambiguous-post",
        },
    );
    assert_eq!(recovered.code, 0, "{}", recovered.envelope);
    assert_eq!(recovered.comment_count, 1, "{}", recovered.log);
    assert_eq!(
        recovered.log.matches("issue comment 42").count(),
        1,
        "{}",
        recovered.log
    );
    assert_eq!(
        recovered.envelope["payload"]["result"]["closeout_url"],
        EXISTING_URL
    );
    assert_eq!(recovered.issue_state, "closed\n");
}

#[test]
fn record_close_recovers_ambiguous_close_when_provider_confirms_closed() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: true,
            partial_label_edit_once: false,
        },
    );

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(envelope["status"], "ok");
    assert_eq!(labels, "state::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert_eq!(log.matches("issue close 42").count(), 1, "{log}");
    let close_offset = log.rfind("issue close 42").expect("close invocation");
    assert!(
        log[close_offset..].contains("issue view 42"),
        "ambiguous close must be followed by an independent state read: {log}"
    );
    assert!(log.contains("--add-label state::closed"), "{log}");
    assert!(log.contains("issue comment 42"), "{log}");
    assert!(log.contains("issue edit 42"), "{log}");
}

#[test]
fn record_close_keeps_closed_label_state_when_comment_write_fails() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::needs-triage\nworkflow::tracking\n",
            repo_labels_json: r#"[{"name":"state::needs-triage","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""},{"name":"workflow::tracking","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: true,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        },
    );

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-comment-post-failed"
    );
    assert_eq!(labels, "workflow::tracking\nstate::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue comment 42"), "{log}");
    let close = log.find("issue close 42").expect("close call");
    let label_edit = log
        .find("issue edit 42 --add-label state::closed --remove-label state::needs-triage")
        .expect("label edit call");
    let comment = log.find("issue comment 42").expect("comment call");
    assert!(close < label_edit && label_edit < comment, "{log}");
    assert_eq!(log.matches("issue edit 42").count(), 1, "{log}");
}

#[test]
fn record_close_live_rejects_contradictory_provider_labels() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case(true);

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(envelope["error"]["code"], "record-close-label-edit-failed");
    assert_eq!(labels, "state::needs-triage\n");
    assert_eq!(issue_state, "closed\n");
    let close = log.find("issue close 42").expect("close call");
    let label_edit = log
        .find("issue edit 42 --add-label state::closed --remove-label state::needs-triage")
        .expect("label edit call");
    assert!(close < label_edit, "{log}");
    assert!(!log.contains("issue comment"), "{log}");
}

#[test]
fn record_close_live_observes_confirmed_provider_labels() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case(false);

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(envelope["payload"]["result"]["operation"], "record.close");
    assert_eq!(envelope["payload"]["result"]["mode"], "live");
    assert_eq!(labels, "state::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert!(log.contains("issue close 42"), "{log}");
    assert!(
        log.contains("issue edit 42 --add-label state::closed --remove-label state::needs-triage"),
        "{log}"
    );
    let label_edit = log
        .find("issue edit 42 --add-label state::closed --remove-label state::needs-triage")
        .expect("label edit call");
    let comment = log.find("issue comment 42").expect("comment call");
    let close = log.find("issue close 42").expect("close call");
    assert!(close < label_edit && label_edit < comment, "{log}");
    assert_eq!(
        envelope["payload"]["result"]["labels"]["confirmed"],
        json!(["state::closed"])
    );
    let dashboard = envelope["payload"]["result"]["final_dashboard"]
        .as_str()
        .expect("final dashboard");
    assert!(dashboard.contains("- Status: complete"), "{dashboard}");
    assert!(
        dashboard.contains("- Current task: complete"),
        "{dashboard}"
    );
    assert!(dashboard.contains("- Next action: none"), "{dashboard}");
    assert!(
        dashboard.contains("#issuecomment-1"),
        "closeout URL must survive provider read-after-write lag: {dashboard}"
    );
}

#[test]
fn record_close_missing_add_label_fails_before_provider_mutations() {
    let (dry_code, dry_envelope, dry_log, dry_labels, dry_issue_state) =
        run_live_record_close_label_case_with(LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: true,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        });
    assert_eq!(dry_code, 0, "{dry_envelope}");
    let dry_plan = &dry_envelope["payload"]["result"]["preview"]["labels"];
    assert_eq!(dry_plan["availability"]["checked"], true);
    assert_eq!(
        dry_plan["availability"]["missing_additions"],
        json!(["state::closed"])
    );
    assert_eq!(dry_labels, "state::ready\n");
    assert_eq!(dry_issue_state, "open\n");
    assert!(dry_log.contains("label list"), "{dry_log}");
    assert!(!dry_log.contains("issue comment"), "{dry_log}");
    assert!(!dry_log.contains("issue close"), "{dry_log}");

    let (code, envelope, log, labels, issue_state) =
        run_live_record_close_label_case_with(LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        });

    assert_ne!(code, 0, "{envelope}");
    assert_eq!(
        envelope["error"]["code"],
        "record-close-label-preflight-failed"
    );
    assert_eq!(labels, "state::ready\n");
    assert_eq!(issue_state, "open\n");
    assert!(log.contains("label list"), "{log}");
    assert!(!log.contains("issue comment"), "{log}");
    assert!(!log.contains("issue close"), "{log}");
    assert!(!log.contains("issue edit 42"), "{log}");
}

#[test]
fn record_close_normalizes_all_existing_state_labels() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\nstate::needs-triage\nworkflow::tracking\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""},{"name":"state::needs-triage","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""},{"name":"workflow::tracking","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        },
    );

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(labels, "workflow::tracking\nstate::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert!(
        log.contains("issue edit 42 --add-label state::closed --remove-label state::needs-triage --remove-label state::ready")
            || log.contains("issue edit 42 --add-label state::closed --remove-label state::ready --remove-label state::needs-triage"),
        "{log}"
    );
    assert_eq!(
        envelope["payload"]["result"]["labels"]["confirmed"],
        json!(["state::closed", "workflow::tracking"])
    );
}

#[test]
fn record_close_live_dry_run_predicts_label_availability_and_final_set() {
    let (code, envelope, log, labels, issue_state) = run_live_record_close_label_case_with(
        LiveCloseLabelCase {
            repo: "sympoies/agent-runtime-kit",
            initial_labels: "state::ready\nworkflow::tracking\n",
            repo_labels_json: r#"[{"name":"state::ready","color":"000000","description":""},{"name":"state::closed","color":"000000","description":""},{"name":"workflow::tracking","color":"000000","description":""}]"#,
            drop_label_mutations: false,
            dry_run: true,
            explicit_remove: false,
            local_label_list_unsupported: false,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        },
    );

    assert_eq!(code, 0, "{envelope}");
    let plan = &envelope["payload"]["result"]["preview"]["labels"];
    assert_eq!(plan["availability"]["checked"], true);
    assert_eq!(plan["availability"]["missing_additions"], json!([]));
    assert_eq!(
        plan["current"],
        json!(["state::ready", "workflow::tracking"])
    );
    assert_eq!(plan["add"], json!(["state::closed"]));
    assert_eq!(plan["remove"], json!(["state::ready"]));
    assert_eq!(
        plan["final"],
        json!(["state::closed", "workflow::tracking"])
    );
    assert_eq!(labels, "state::ready\nworkflow::tracking\n");
    assert_eq!(issue_state, "open\n");
    assert!(log.contains("label list"), "{log}");
    assert!(!log.contains("issue comment"), "{log}");
    assert!(!log.contains("issue close"), "{log}");
}

#[test]
fn record_close_local_normalizes_labels_without_a_repository_catalog() {
    let (code, envelope, log, labels, issue_state) =
        run_live_record_close_label_case_with(LiveCloseLabelCase {
            repo: "local:demo",
            initial_labels: "state::ready\nworkflow::tracking\n",
            repo_labels_json: "[]",
            drop_label_mutations: false,
            dry_run: false,
            explicit_remove: false,
            local_label_list_unsupported: true,
            fail_comment: false,
            automation_label_after_edit: false,
            fail_close_after_mutation: false,
            partial_label_edit_once: false,
        });

    assert_eq!(code, 0, "{envelope}");
    assert_eq!(labels, "workflow::tracking\nstate::closed\n");
    assert_eq!(issue_state, "closed\n");
    assert!(!log.contains("label list"), "{log}");
    assert_eq!(
        envelope["payload"]["result"]["labels"]["availability"]["checked"],
        false
    );
}

#[test]
fn forge_cli_stub_preserves_repeated_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let labels_path = tmp.path().join("labels.txt");
    fs::write(&labels_path, "remove::one\nRemove Label\nkeep::me\n").expect("seed labels");
    let labels_s = labels_path.to_string_lossy().to_string();

    let out = nils_test_support::cmd::run(
        &stub.path().join("forge-cli"),
        &[
            "--format",
            "json",
            "--provider",
            "github",
            "--repo",
            "sympoies/nils-cli",
            "issue",
            "edit",
            "42",
            "--remove-label",
            "remove::one",
            "--remove-label",
            "Remove Label",
            "--add-label",
            "add::one",
            "--add-label",
            "Add Label",
        ],
        &[("FORGE_CLI_STUB_LABELS_FILE", labels_s.as_str())],
        None,
    );

    assert_eq!(out.code, 0, "{}", out.stderr_text());
    assert_eq!(
        fs::read_to_string(labels_path).expect("provider labels"),
        "keep::me\nadd::one\nAdd Label\n"
    );
    assert_eq!(
        out.stdout_json()["data"]["labels"],
        json!(["keep::me", "add::one", "Add Label"])
    );
}

#[test]
fn record_post_state_with_payload_file_renders_v2_marker_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "scope",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.post");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment_body in dry-run");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
}

#[test]
fn record_post_state_summary_file_is_rendered_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        "- Updated runtime-kit skills to the v3 surface.\n",
    )
    .expect("write summary");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--summary-file",
        summary.to_str().expect("summary str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert!(
        body.contains("- Updated runtime-kit skills to the v3 surface."),
        "{body}"
    );
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

#[test]
fn record_post_live_rejects_local_path_from_summary_file_before_provider_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        "- Evidence: /Users/dev/Project/private/rendered.md\n",
    )
    .expect("write summary");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "post",
            "--issue",
            "217",
            "--kind",
            "state",
            "--payload-file",
            payload.to_str().expect("payload str"),
            "--summary-file",
            summary.to_str().expect("summary str"),
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    // Post-consolidation the local-path privacy guard is enforced by forge-cli
    // on the `issue comment` write (not a plan-issue-side pre-flight guard), so
    // forge-cli IS invoked and rejects the rendered comment; the adapter
    // surfaces forge-cli's `local_path_present` error through
    // `record-post-comment-post-failed`. The adapter surfaces only
    // `code: message`, so the message carries the generic class line; the
    // per-path `$HOME/...` suggestion lives in forge-cli's unsurfaced `detail`.
    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "record-post-comment-post-failed");
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("machine-local home path"),
        "message should name local-path class: {message}"
    );
    assert!(!message.contains("/Users/dev"), "{message}");
}

#[test]
fn record_post_live_rejects_summary_payload_carrier_before_provider_mutation() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        concat!(
            "Quoted prior lifecycle comment:\n\n",
            "```plan-issue-record-payload\n",
            "{\"schema\":\"plan-issue-record.payload.v2\",\"role\":\"state\",\"profile\":\"tracking\",\"data\":{\"status\":\"complete\",\"tasks\":[],\"prs\":[],\"blockers\":[],\"links\":{}}}\n",
            "```\n",
        ),
    )
    .expect("write summary");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "post",
            "--issue",
            "217",
            "--kind",
            "state",
            "--payload-file",
            payload.to_str().expect("payload str"),
            "--summary-file",
            summary.to_str().expect("summary str"),
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(
        parsed["error"]["code"],
        "record-post-payload-carrier-conflict"
    );
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("multiple plan-issue-record-payload carriers"),
        "{message}"
    );
    assert!(
        !log_path.exists(),
        "gh must not run when rendered comment has multiple payload carriers"
    );
}

#[test]
fn record_post_live_rejects_summary_payload_carrier_before_unclosed_details() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let payload = tmp.path().join("state.json");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "summary surface",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &summary,
        concat!(
            "Quoted prior lifecycle comment:\n\n",
            "```plan-issue-record-payload\n",
            "{\"schema\":\"plan-issue-record.payload.v2\",\"role\":\"state\",\"profile\":\"tracking\",\"data\":{\"status\":\"complete\",\"tasks\":[],\"prs\":[],\"blockers\":[],\"links\":{}}}\n",
            "```\n\n",
            "<details>\n",
            "<summary>Unclosed quoted details</summary>\n"
        ),
    )
    .expect("write summary");

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "post",
            "--issue",
            "217",
            "--kind",
            "state",
            "--payload-file",
            payload.to_str().expect("payload str"),
            "--summary-file",
            summary.to_str().expect("summary str"),
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    assert_eq!(out.code, 64, "stdout={}", out.stdout_text());
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(
        parsed["error"]["code"],
        "record-post-payload-carrier-conflict"
    );
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("multiple plan-issue-record-payload carriers"),
        "{message}"
    );
    assert!(
        !log_path.exists(),
        "gh must not run when rendered comment has multiple payload carriers"
    );
}

#[test]
fn record_post_state_execution_state_file_collapses_non_final_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "ledger surface",
            "current": "working",
            "next_action": "continue",
            "tasks": [{"id": "1.1", "status": "pending", "title": "Demo task"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n## Execution State\n\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Demo task |\n\n## Validation\n\n| Command | Status |\n| --- | --- |\n| `true` | pass |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: in-progress\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | pending | Demo task |\n\n",
            "</details>\n\n",
            "## Validation\n\n",
            "| Command | Status |\n",
            "| --- | --- |\n",
            "| `true` | pass |\n\n",
        ),
    );
    let details_start = body.find("<details>").expect("details start");
    let validation_start = body.find("## Validation").expect("validation heading");
    let payload_start = body
        .find("<!-- plan-issue-record-payload:hex:")
        .expect("payload marker");
    assert!(details_start < validation_start, "{body}");
    assert!(validation_start < payload_start, "{body}");
}

#[test]
fn record_post_state_execution_state_file_composes_summary_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    let summary = tmp.path().join("summary.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "ledger surface",
            "current": "working",
            "next_action": "continue",
            "tasks": [{"id": "1.1", "status": "pending", "title": "Demo task"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n## Execution State\n\n- Status: in-progress\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Demo task |\n",
    )
    .expect("write execution state");
    fs::write(&summary, "Checkpoint summary above the ledger.\n").expect("write summary");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
        "--summary-file",
        summary.to_str().expect("summary str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "Checkpoint summary above the ledger.\n\n",
            "- Status: in-progress\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | pending | Demo task |\n\n",
            "</details>\n\n",
        ),
    );
}

#[test]
fn record_post_state_execution_state_file_expands_final_in_dry_run() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "complete",
            "target_scope": "ledger surface",
            "current": "done",
            "next_action": "closeout",
            "tasks": [{"id": "1.1", "status": "done", "title": "Demo task"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n## Execution State\n\n- Status: complete\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | done | Demo task |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: complete\n\n",
            "## Task Ledger\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | done | Demo task |\n\n",
        ),
    );
}

#[test]
fn record_post_state_execution_state_file_preserves_execution_metadata_fields() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "plan issue lifecycle comment visibility",
            "current": "implement Sprint 1 in sympoies/nils-cli",
            "next_action": "add lifecycle visible rendering support",
            "tasks": [{"id": "1.1", "status": "pending", "title": "Renderer"}],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(
        &execution_state,
        "# Execution State: Plan Issue Lifecycle Comment Visibility\n\n<!-- execute-from-tracking-issue:state:v1 -->\n## Execution State\n\n- Status: tracking issue opened\n- Profile: tracking\n- Target scope: make plan-issue lifecycle comments visibly include detailed state, validation, review, session, and closeout evidence\n- Current task: implement Sprint 1 in sympoies/nils-cli.\n- Next task: add lifecycle visible rendering support to plan-issue record post and record close.\n- Last updated: 2026-05-25\n- Branch: feat/plan-issue-state-visibility\n- Source document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n- Plan document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n- Review source: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-review-source.md\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Renderer |\n",
    )
    .expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "115",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["comment_body"]
        .as_str()
        .expect("comment body");
    assert_comment_visible_prefix(
        body,
        concat!(
            "<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n",
            "## Execution State\n\n",
            "- Profile: tracking\n",
            "- Status: tracking issue opened\n",
            "- Target scope: make plan-issue lifecycle comments visibly include detailed state, validation, review, session, and closeout evidence\n",
            "- Current task: implement Sprint 1 in sympoies/nils-cli.\n",
            "- Next task: add lifecycle visible rendering support to plan-issue record post and record close.\n",
            "- Last updated: 2026-05-25\n",
            "- Branch: feat/plan-issue-state-visibility\n",
            "- Source document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n",
            "- Plan document: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-plan.md\n",
            "- Review source: docs/plans/plan-issue-lifecycle-comment-visibility/plan-issue-lifecycle-comment-visibility-review-source.md\n\n",
            "## Task Ledger\n\n",
            "<details>\n",
            "<summary>Show task ledger</summary>\n\n",
            "| ID | Status | Task |\n",
            "| --- | --- | --- |\n",
            "| 1.1 | pending | Renderer |\n\n",
            "</details>\n\n",
        ),
    );
}

#[test]
fn record_post_execution_state_file_rejects_empty_document() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");
    fs::write(&execution_state, "   \n\n").expect("write execution state");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);
    assert_ne!(out.code, 0);
    assert!(
        out.stdout_text()
            .contains("record-post-execution-state-empty"),
        "{}",
        out.stdout_text()
    );
}

#[test]
fn record_post_execution_state_file_requires_state_kind_and_task_ledger() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("validation.json");
    let execution_state = tmp.path().join("state.md");
    fs::write(
        &payload,
        json!({"overall": "pass", "commands": [], "waivers": []}).to_string(),
    )
    .expect("write payload");
    fs::write(&execution_state, "# State\n").expect("write execution state");

    let wrong_kind = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "validation",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);
    assert_ne!(wrong_kind.code, 0);
    assert!(
        wrong_kind
            .stdout_text()
            .contains("record-post-execution-state-file-kind-invalid"),
        "{}",
        wrong_kind.stdout_text()
    );

    let state_payload = tmp.path().join("state.json");
    fs::write(
        &state_payload,
        json!({
            "status": "in-progress",
            "tasks": [],
            "prs": [],
            "blockers": [],
            "links": {}
        })
        .to_string(),
    )
    .expect("write state payload");
    let missing_ledger = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        state_payload.to_str().expect("payload str"),
        "--execution-state-file",
        execution_state.to_str().expect("execution state str"),
    ]);
    assert_ne!(missing_ledger.code, 0);
    assert!(
        missing_ledger
            .stdout_text()
            .contains("record-post-execution-state-task-ledger-missing"),
        "{}",
        missing_ledger.stdout_text()
    );
}

#[test]
fn record_post_state_rejects_payload_that_cannot_drive_dashboard() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({
            "status": "in-progress",
            "target_scope": "schema drift",
            "current": "PRs are open as drafts",
            "next_action": "review draft PRs",
            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
            "prs": [
                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open"}
            ],
            "blockers": [
                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
            ],
            "links": {}
        })
        .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
    ]);

    assert_ne!(out.code, 0, "invalid state payload must fail");
    assert!(
        out.stderr_text()
            .contains("record-post-payload-schema-invalid")
            || out
                .stdout_text()
                .contains("record-post-payload-schema-invalid"),
        "expected schema-invalid error: stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
}

#[test]
fn record_post_rejects_source_plan_and_closeout_kinds() {
    for kind in ["source", "plan", "closeout"] {
        let out = common::run_plan_issue_local(&[
            "--format", "json", "record", "post", "--issue", "1", "--kind", kind,
        ]);
        assert_ne!(out.code, 0, "kind {kind} should be rejected");
        assert!(
            out.stderr_text().contains("record-post-")
                || out.stdout_text().contains("record-post-"),
            "expected record-post error for kind {kind}: stdout={} stderr={}",
            out.stdout_text(),
            out.stderr_text()
        );
    }
}

#[test]
fn record_repair_dashboard_rejects_malformed_state_payload_instead_of_pending() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema drift",
                            "current": "PRs are open as drafts",
                            "next_action": "review draft PRs",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [
                                {"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "draft-open-green"}
                            ],
                            "blockers": [
                                {"code": "live-home-drift", "status": "open", "detail": "extra surface"}
                            ],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "malformed state payload must fail repair");
    assert!(
        out.stderr_text().contains("malformed payload")
            || out.stdout_text().contains("malformed payload"),
        "expected malformed payload error: stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
}

#[test]
fn record_repair_dashboard_allows_new_valid_state_to_supersede_old_malformed_state() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: pending\n").expect("write body");

    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-old",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "old schema drift",
                            "prs": [{"ref": "owner/repo#9", "status": "draft-open"}],
                            "blockers": [{"code": "x"}],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                },
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state-new",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "schema repaired",
                            "current": "latest valid state",
                            "next_action": "continue",
                            "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                            "prs": [{"ref": "owner/repo#9", "url": "https://github.com/owner/repo/pull/9", "status": "open"}],
                            "blockers": ["older malformed state superseded"],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T11:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    assert!(
        dashboard.contains("- Target scope: schema repaired"),
        "{dashboard}"
    );
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-state-new"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_renders_canonical_dashboard_from_body_and_comments() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(
        &body_path,
        "## Current Dashboard\n\n- Status: in-progress\n",
    )
    .expect("write body");

    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state-1",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({
                        "status": "in-progress",
                        "target_scope": "sample plan",
                        "current": "Sprint 2 in progress",
                        "next_action": "land Sprint 2",
                        "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                        "prs": [{"ref": "owner/repo#1", "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "abc"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            }
        ]
    });
    fs::write(
        &comments_path,
        serde_json::to_string(&comments).expect("json"),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let dashboard = parsed["payload"]["result"]["dashboard_markdown"]
        .as_str()
        .expect("dashboard markdown");
    assert!(
        dashboard.starts_with("<!-- plan-issue-record-identity:v1:hex:")
            && dashboard.contains("\n\n## Current Dashboard"),
        "{dashboard}"
    );
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
    // Source URL from latest audit evidence should appear in Durable Record.
    assert!(
        dashboard.contains("https://github.com/owner/repo/issues/9#issuecomment-source"),
        "{dashboard}"
    );
}

#[test]
fn record_repair_dashboard_reuses_resolved_self_hosted_repo_for_edit() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let comments = json!([
        {
            "url": "https://gitlab.corp.example/acme/widgets/-/issues/217#note_1",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({
                    "status": "in-progress",
                    "target_scope": "self-hosted repair",
                    "current": "repair dashboard",
                    "next_action": "continue",
                    "tasks": [],
                    "prs": [],
                    "blockers": [],
                    "links": {}
                }),
            ),
            "created_at": "2026-05-23T10:00:00Z"
        }
    ]);
    let body_json = json!("## Current Dashboard\n\n- Status: stale\n").to_string();
    let comments_json = comments.to_string();
    let log_path = tmp.path().join("forge-cli.log");
    let capture_path = tmp.path().join("dashboard.md");
    let log_s = log_path.to_string_lossy().to_string();
    let capture_s = capture_path.to_string_lossy().to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "https://gitlab.corp.example/acme/widgets",
            "record",
            "repair-dashboard",
            "--issue",
            "217",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &capture_s),
            ],
        ),
    );

    assert_eq!(
        out.code,
        0,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let log = fs::read_to_string(log_path).expect("provider log");
    let issue_lines = log
        .lines()
        .filter(|line| line.contains("issue view 217") || line.contains("issue edit 217"))
        .collect::<Vec<_>>();
    assert_eq!(issue_lines.len(), 2, "{log}");
    assert!(
        issue_lines.iter().all(|line| {
            line.contains("--host gitlab.corp.example") && line.contains("--repo acme/widgets")
        }),
        "repair edit lost self-hosted authority: {log}"
    );
}

#[test]
fn record_repair_dashboard_live_rejects_local_path_in_rendered_dashboard_before_edit() {
    let tmp = TempDir::new().expect("tempdir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");
    let log_s = log_path.to_string_lossy().to_string();

    let comments = json!([
        {
            "url": "https://github.com/owner/repo/issues/9#issuecomment-state-1",
            "body": v2_comment_body(
                "state",
                "tracking",
                json!({
                    "status": "in-progress",
                    "target_scope": "/Users/dev/Project/private/dashboard",
                    "current": "repair dashboard",
                    "next_action": "block unsafe payload",
                    "tasks": [{"id": "1.1", "status": "done", "title": "x"}],
                    "prs": [],
                    "blockers": [],
                    "links": {}
                }),
            ),
            "created_at": "2026-05-23T10:00:00Z"
        }
    ]);
    // The issue body is clean (`issue view` is a read with no guard); the local
    // path rides in the state comment's `target_scope`, so the *rendered*
    // dashboard carries it into the `issue edit` write — which forge-cli's
    // `local_path_present` guard rejects.
    let body_json = json!("## Current Dashboard\n\n- Status: stale\n").to_string();
    let comments_json = comments.to_string();

    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--repo",
            "sympoies/nils-cli",
            "record",
            "repair-dashboard",
            "--issue",
            "217",
        ],
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                ("FORGE_CLI_STUB_VIEW_BODY_JSON", &body_json),
                ("FORGE_CLI_STUB_VIEW_COMMENTS_JSON", &comments_json),
            ],
        ),
    );

    // Post-consolidation the local-path privacy guard is enforced by forge-cli
    // on the `issue edit` write, surfaced through the adapter as the
    // `record-repair-edit-failed` runtime error. The adapter surfaces only
    // forge-cli's `code: message`, so the message carries the generic
    // machine-local-home-path class line (the per-path `$HOME/...` suggestion
    // lives in forge-cli's `detail`, which the adapter does not surface).
    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "record-repair-edit-failed");
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(
        message.contains("machine-local home path"),
        "message should name local-path class: {message}"
    );
    assert!(!message.contains("/Users/dev"), "{message}");

    // `issue view` (read) ran; the `issue edit` write was attempted and
    // rejected by forge-cli, so both verbs appear in the log.
    let log = fs::read_to_string(&log_path).expect("read forge-cli log");
    assert!(log.contains("issue view 217"), "{log}");
    assert!(log.contains("issue edit 217"), "{log}");
}

#[test]
fn record_repair_dashboard_out_writes_local_dashboard_file() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    let out_path = tmp.path().join("dashboard.md");
    fs::write(&body_path, "## Current Dashboard\n\n- Status: stale\n").expect("write body");
    fs::write(
        &comments_path,
        json!({
            "comments": [
                {
                    "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                    "body": v2_comment_body(
                        "state",
                        "tracking",
                        json!({
                            "status": "in-progress",
                            "target_scope": "repair out",
                            "current": "refresh dashboard",
                            "next_action": "continue",
                            "tasks": [],
                            "prs": [],
                            "blockers": [],
                            "links": {}
                        }),
                    ),
                    "created_at": "2026-05-23T10:00:00Z"
                }
            ]
        })
        .to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "repair-dashboard",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
        "--out",
        out_path.to_str().expect("out path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "local");
    assert_eq!(
        result["out_path"],
        out_path.to_string_lossy().as_ref(),
        "{result}"
    );
    let dashboard = fs::read_to_string(&out_path).expect("read dashboard");
    assert!(dashboard.starts_with("## Current Dashboard"), "{dashboard}");
    assert!(dashboard.contains("- Status: in-progress"), "{dashboard}");
}

#[test]
fn record_close_requires_non_empty_approval() {
    let out =
        common::run_plan_issue_local(&["--format", "json", "record", "close", "--issue", "9"]);
    assert_ne!(out.code, 0, "missing --approval should fail");
    assert!(
        out.stderr_text().contains("record-close-missing-approval")
            || out.stdout_text().contains("record-close-missing-approval"),
        "stderr: {} stdout: {}",
        out.stderr_text(),
        out.stdout_text()
    );
}

fn build_closeout_evidence(linked_pr_ref: &str) -> Value {
    build_closeout_evidence_for_profile(linked_pr_ref, "tracking")
}

fn build_closeout_evidence_for_profile(linked_pr_ref: &str, profile: &str) -> Value {
    json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    profile,
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1234"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    profile,
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1234"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    profile,
                    json!({
                        "status": "complete",
                        "target_scope": "sample plan",
                        "current": "complete",
                        "next_action": "closeout",
                        "tasks": [
                            {"id": "1.1", "status": "done", "title": "x"},
                            {"id": "1.2", "status": "deferred", "title": "y"},
                        ],
                        "prs": [{"ref": linked_pr_ref, "url": "https://github.com/owner/repo/pull/1", "status": "merged"}],
                        "blockers": [],
                        "links": {}
                    }),
                ),
                "created_at": "2026-05-23T10:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-session",
                "body": v2_comment_body(
                    "session",
                    profile,
                    json!({
                        "summary": "implementation session completed",
                        "highlights": ["state, validation, and review evidence recorded"]
                    }),
                ),
                "created_at": "2026-05-23T10:30:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-validation",
                "body": v2_comment_body(
                    "validation",
                    profile,
                    json!({
                        "overall": "pass",
                        "commands": [{"command": "cargo test", "status": "pass"}],
                        "waivers": []
                    }),
                ),
                "created_at": "2026-05-23T11:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-review",
                "body": v2_comment_body(
                    "review",
                    profile,
                    json!({
                        "decision": "approve",
                        "lenses": ["testing", "maintainability"],
                        "findings": [],
                    }),
                ),
                "created_at": "2026-05-23T12:00:00Z"
            }
        ]
    })
}

fn remove_session_comment(comments: &mut Value) {
    comments["comments"]
        .as_array_mut()
        .expect("comments array")
        .retain(|comment| {
            !comment["body"]
                .as_str()
                .unwrap_or_default()
                .contains("role=session")
        });
}

#[test]
fn record_close_body_file_mode_blocks_unresolved_linked_pr() {
    let tmp = TempDir::new().expect("tempdir");
    let body_path = tmp.path().join("body.md");
    let comments_path = tmp.path().join("comments.json");
    fs::write(&body_path, "## Current Dashboard\n").expect("write body");
    fs::write(
        &comments_path,
        build_closeout_evidence("owner/repo#1").to_string(),
    )
    .expect("write comments");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--body-file",
        body_path.to_str().expect("body path"),
        "--comments-json",
        comments_path.to_str().expect("comments path"),
    ]);

    assert_ne!(out.code, 0, "missing provider PR evidence should block");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged without PR evidence: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_strict_gate_with_complete_v2_evidence() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    let body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body");
    assert!(
        body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{body}"
    );
    assert!(
        !body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{body}"
    );
    assert!(
        body.contains("<!-- plan-issue-record-payload:hex:"),
        "{body}"
    );
    let audit = audit_single_comment_body(body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(closeout["linked_prs"][0]["merge_sha"], "deadbeefcafebabe");
    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard");
    assert!(
        final_dashboard.starts_with("<!-- plan-issue-record-identity:v1:hex:")
            && final_dashboard.contains("\n\n## Final Dashboard"),
        "{final_dashboard}"
    );
}

#[test]
fn record_close_dispatch_fixture_passes_visible_read_back() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    write_fixture_files(
        &fixture,
        "## Current Dashboard\n\n- Status: in-progress\n",
        &build_closeout_evidence_for_profile("owner/repo#1", "dispatch"),
    );
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let close = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--profile",
        "dispatch",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_eq!(close.code, 0, "stderr: {}", close.stderr_text());
    let closeout_body =
        close.stdout_json()["payload"]["result"]["preview"]["closeout_comment_body"]
            .as_str()
            .expect("closeout body")
            .to_string();
    assert!(
        closeout_body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=dispatch -->"),
        "{closeout_body}"
    );

    let comments_json = tmp.path().join("closeout-comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [{
                "body": closeout_body,
                "url": "https://github.com/owner/repo/issues/9#issuecomment-closeout"
            }]
        })
        .to_string(),
    )
    .expect("write closeout comments");
    let audit = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "audit",
        "--comments-json",
        comments_json.to_str().expect("comments path"),
        "--profile",
        "dispatch",
        "--expect-visible",
    ]);

    assert_eq!(audit.code, 0, "stderr: {}", audit.stderr_text());
    let envelope = audit.stdout_json();
    assert_eq!(
        envelope["payload"]["result"]["audit"]["recognized_count"], 1,
        "{envelope}"
    );
    let visible = &envelope["payload"]["result"]["visible"];
    assert_eq!(visible["overall_pass"], true, "{envelope}");
    assert_eq!(visible["codes"], json!([]), "{envelope}");
    let closeout = visible["roles"]
        .as_array()
        .expect("visible roles")
        .iter()
        .find(|role| role["role"] == "closeout")
        .expect("closeout role");
    assert_eq!(closeout["present"], true, "{envelope}");
    assert_eq!(closeout["checked"], true, "{envelope}");
    assert_eq!(closeout["pass"], true, "{envelope}");
}

#[test]
fn record_close_fixture_blocks_when_session_comment_missing() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let mut comments = build_closeout_evidence("owner/repo#1");
    remove_session_comment(&mut comments);
    let body = "## Current Dashboard\n\n- Status: complete\n- Latest session: pending\n\n## Session Log\n\n- Notes embedded in state only.\n";
    write_fixture_files(&fixture, body, &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "missing session must block closeout");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("session-missing"),
        "expected session-missing, got: {joined}"
    );
}

#[test]
fn record_close_fixture_blocks_when_linked_pr_not_merged() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict gate");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}

#[test]
fn record_close_fixture_blocks_when_review_request_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    // Replace the review entry in the evidence stack with a
    // request-changes decision.
    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    let last_index = comments_list.len() - 1;
    comments_list[last_index] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-review-rej",
        "body": v2_comment_body(
            "review",
            "tracking",
            json!({"decision": "request-changes", "findings": []}),
        ),
        "created_at": "2026-05-23T12:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("review-rejected"),
        "expected review-rejected: {joined}"
    );
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_zero_required() {
    // Regression for sympoies/nils-cli#502:
    // PR merged, zero required checks, one non-required check failed.
    // Strict closeout gate must not block on non-required failures.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n\n- Status: in-progress\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "deadbeefcafebabe"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 0},
            "nonRequiredFailures": ["scripts/ci/all.sh"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "https://github.com/owner/repo/issues/9#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    let preview = &result["preview"];
    assert!(
        preview["blocked_codes"]
            .as_array()
            .expect("array")
            .is_empty(),
        "blocked_codes should be empty: {}",
        preview["blocked_codes"]
    );
    let linked = &result["linked_prs"][0];
    assert_eq!(linked["required_count"], 0);
    assert_eq!(linked["required_state"], "pass");
    assert_eq!(linked["non_required_failures"][0], "scripts/ci/all.sh");

    // sympoies/nils-cli#561 follow-up: rendered closeout-comment table
    // must label the zero-required case `none required`, not `unknown`
    // and not `pass (0)`.
    let body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body");
    assert!(
        body.contains("| none required |"),
        "expected `none required` in closeout body, got: {body}"
    );
    assert!(
        !body.contains("| unknown |"),
        "expected no `unknown` cell in closeout body, got: {body}"
    );
}

#[test]
fn record_close_fixture_passes_with_non_required_failure_when_required_pass() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "success", "count": 3},
            "nonRequiredFailures": ["lint-experimental"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
}

#[test]
fn record_close_fixture_blocks_with_linked_pr_checks_failed_when_required_fail() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "requiredCheckRollup": {"state": "failure", "count": 2},
            "nonRequiredFailures": [],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "required-check failure must block");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed: {joined}"
    );
    assert!(
        !joined.contains("linked-pr-not-merged"),
        "must not collapse into linked-pr-not-merged: {joined}"
    );
}

#[test]
fn record_close_fixture_override_passes_when_required_unknown_aggregate_fails() {
    // When the adapter cannot resolve required-check state (`requiredCheckRollup`
    // absent), the gate stays conservative and blocks on aggregate failure.
    // The override flag unblocks it and records evidence in the closeout body.
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    write_fixture_files(&fixture, body, &build_closeout_evidence("owner/repo#1"));
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "failure"},
            "nonRequiredFailures": ["opt-in/lint"],
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    // Without the override → blocked.
    let blocked = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(blocked.code, 0, "conservative block expected");
    assert!(
        format!("{}\n{}", blocked.stderr_text(), blocked.stdout_text())
            .contains("linked-pr-checks-failed"),
        "expected linked-pr-checks-failed under unknown required state"
    );

    // With the override + reason → passes and records evidence.
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--allow-non-required-check-failure",
        "--allow-non-required-check-failure-reason",
        "operator verified opt-in/lint is non-required",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(
        out.code,
        0,
        "override should unblock: {}",
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    let body = parsed["payload"]["result"]["preview"]["closeout_comment_body"]
        .as_str()
        .expect("closeout body")
        .to_string();
    assert!(
        body.contains("non-required-check failure override"),
        "expected override summary in body: {body}"
    );
    let audit = audit_single_comment_body(&body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    let override_block = &closeout["non_required_check_override"];
    assert_eq!(
        override_block["reason"], "operator verified opt-in/lint is non-required",
        "override block reason recorded"
    );
    assert!(
        override_block["observed_non_required_failures"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|item| item == "owner/repo#1: opt-in/lint")),
        "expected observed failure list to include opt-in/lint: {override_block}"
    );
}

#[test]
fn record_close_fixture_blocks_when_state_not_complete() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let mut comments = build_closeout_evidence("owner/repo#1");
    let comments_list = comments["comments"].as_array_mut().expect("comments array");
    // Replace the state entry (index 2) with status=in-progress.
    comments_list[2] = json!({
        "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
        "body": v2_comment_body(
            "state",
            "tracking",
            json!({
                "status": "in-progress",
                "target_scope": "x",
                "tasks": [],
                "prs": [],
                "blockers": [],
                "links": {}
            }),
        ),
        "created_at": "2026-05-23T10:00:00Z"
    });
    write_fixture_files(&fixture, "## Current Dashboard\n", &comments);
    write_pr_fixture(
        &fixture,
        "owner/repo",
        1,
        json!({
            "state": "MERGED",
            "mergeCommit": {"oid": "abc"},
            "statusCheckRollup": {"state": "success"},
            "url": "https://github.com/owner/repo/pull/1"
        }),
    );

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "9",
        "--linked-pr",
        "owner/repo#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_ne!(out.code, 0);
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("state-not-complete"),
        "expected state-not-complete: {joined}"
    );
}

#[test]
fn record_open_fixture_mode_returns_v2_evidence_urls() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");

    let body = "## Current Dashboard\n";
    let comments = json!({
        "comments": [
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-source",
                "body": v2_comment_body(
                    "source",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-discussion-source.md", "commit": "src1"}),
                ),
                "created_at": "2026-05-23T09:00:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-plan",
                "body": v2_comment_body(
                    "plan",
                    "tracking",
                    json!({"path": "docs/plans/sample/sample-plan.md", "commit": "pln1"}),
                ),
                "created_at": "2026-05-23T09:01:00Z"
            },
            {
                "url": "https://github.com/owner/repo/issues/9#issuecomment-state",
                "body": v2_comment_body(
                    "state",
                    "tracking",
                    json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}}),
                ),
                "created_at": "2026-05-23T09:02:00Z"
            }
        ]
    });
    write_fixture_files(&fixture, body, &comments);

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "open",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--title",
        "Sample Plan",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "fixture");
    let comments_result = &result["comments"];
    assert_eq!(
        comments_result["source"],
        "https://github.com/owner/repo/issues/9#issuecomment-source"
    );
    assert_eq!(
        comments_result["plan"],
        "https://github.com/owner/repo/issues/9#issuecomment-plan"
    );
    assert_eq!(
        comments_result["state"],
        "https://github.com/owner/repo/issues/9#issuecomment-state"
    );
}

#[test]
fn record_post_state_fixture_returns_rendered_body_without_provider_call() {
    let tmp = TempDir::new().expect("tempdir");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(&fixture).expect("create fixture");
    write_fixture_files(&fixture, "## Current Dashboard\n", &json!({"comments": []}));
    let payload = tmp.path().join("payload.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "post",
        "--issue",
        "9",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload str"),
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["kind"], "state");
    let body = result["comment_body"]
        .as_str()
        .expect("comment body in fixture mode");
    assert!(
        body.contains("<!-- plan-issue-record:v2 role=state profile=tracking -->"),
        "{body}"
    );
}

fn record_open_dry_run_forge_stub() -> &'static str {
    r#"#!/usr/bin/env bash
echo "record_open_dry_run_forge_stub should not be called" >&2
exit 1
"#
}

fn dry_run_cmd_options(stub_dir: &Path) -> CmdOptions {
    common::plan_issue_cmd_options()
        .with_env_remove_prefix("FORGE_CLI_STUB_")
        .with_path_prepend(stub_dir)
}

#[test]
fn record_open_dry_run_returns_preview_without_gh_calls() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", record_open_dry_run_forge_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.open");
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["dry_run"], true);
    let preview = &result["preview"];
    assert_eq!(preview["plan_title"], "Plan: Sample Plan");
    let issue_body = preview["issue_body_markdown"].as_str().expect("issue body");
    assert!(
        issue_body.starts_with("<!-- plan-issue-record-identity:v1:hex:"),
        "record open must make the bundle identity durable before the first comment:\n{issue_body}"
    );
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.starts_with("<!-- plan-issue-record:v2 role=source profile=tracking -->"),
        "{source_comment}"
    );
    let plan_comment = preview["comments"]["plan"].as_str().expect("plan comment");
    let plan_audit = audit_single_comment_body(plan_comment);
    assert_eq!(
        plan_audit["evidence"]["plan"]["payload"]["data"]["title"], "Plan: Sample Plan",
        "record open must persist the authored title in plan snapshot evidence"
    );
    let state_comment = preview["comments"]["state"]
        .as_str()
        .expect("state comment");
    for (label, comment) in [
        ("source", source_comment),
        ("plan", plan_comment),
        ("state", state_comment),
    ] {
        assert!(
            !comment.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
            "{label} comment should not visibly leak payload JSON:\n{comment}"
        );
    }
    assert!(
        state_comment.contains("# Sample Execution State"),
        "{state_comment}"
    );
    assert!(
        state_comment.contains("- Status: pending"),
        "{state_comment}"
    );
    assert!(
        !state_comment.contains("Initial execution state seeded"),
        "{state_comment}"
    );

    let comments_json = repo.path().join("comments.json");
    fs::write(
        &comments_json,
        json!({
            "comments": [
                {"body": source_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-source"},
                {"body": plan_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-plan"},
                {"body": state_comment, "url": "https://github.com/owner/repo/issues/1#issuecomment-state"}
            ]
        })
        .to_string(),
    )
    .expect("write comments json");

    let audit = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "audit",
            "--comments-json",
            comments_json.to_str().expect("comments path"),
            "--profile",
            "tracking",
        ],
        &opts,
    );
    assert_eq!(audit.code, 0, "stderr: {}", audit.stderr_text());
    let parsed_audit: Value = serde_json::from_str(&audit.stdout_text()).expect("audit json");
    let audit_result = &parsed_audit["payload"]["result"]["audit"];
    assert_eq!(audit_result["recognized_count"], 3);
    assert_eq!(
        audit_result["missing_required"],
        json!([]),
        "{audit_result}"
    );
}

/// Write the minimal source/plan/execution-state trio used by the `record open`
/// dry-run tests into `bundle` (created if needed).
fn write_sample_bundle(bundle: &Path) {
    fs::create_dir_all(bundle).expect("create bundle dir");
    fs::write(
        bundle.join("sample-discussion-source.md"),
        "# Source\n\n- Decision: implement v2 lifecycle.\n",
    )
    .expect("write source");
    fs::write(
        bundle.join("sample-plan.md"),
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        bundle.join("sample-execution-state.md"),
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
}

fn commit_all(repo: &Path) {
    use nils_test_support::git::git;
    git(repo, &["add", "."]);
    git(
        repo,
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );
}

/// Regression for the false `record-open-uncommitted` tracked in
/// graysurf/plan-tracking-testbed#48: a committed bundle passed through a
/// *relative* `--bundle` must resolve its commit, not be misread as
/// uncommitted. Before the fix `last_commit_for_path` ran `git log` from the
/// bundle's parent dir but passed the full relative path as the pathspec, which
/// re-anchored under that subdir cwd and matched nothing.
#[test]
fn record_open_dry_run_resolves_relative_bundle() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", record_open_dry_run_forge_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    commit_all(repo.path());

    // Relative `--bundle`, resolved against the process cwd (the repo root).
    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
        ],
        &opts,
    );
    assert_eq!(
        out.code,
        0,
        "relative --bundle must succeed; stderr: {}",
        out.stderr_text()
    );
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let preview = &parsed["payload"]["result"]["preview"];
    let source_commit = preview["source_commit"]
        .as_str()
        .expect("source_commit string");
    let plan_commit = preview["plan_commit"].as_str().expect("plan_commit string");
    assert!(
        !source_commit.is_empty(),
        "committed source must resolve a commit: {preview}"
    );
    assert!(
        !plan_commit.is_empty(),
        "committed plan must resolve a commit: {preview}"
    );
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.contains("- Commit: `"),
        "committed source snapshot should render a Commit line:\n{source_comment}"
    );
    assert!(
        source_comment.contains("- Snapshot mode: local committed Markdown"),
        "committed snapshot should be labeled committed:\n{source_comment}"
    );
}

/// Companion to graysurf/plan-tracking-testbed#48: `--allow-dirty` must actually
/// bypass the commit check, as its error hint advertises. A never-committed
/// bundle is rejected by default but allowed through with `--allow-dirty`,
/// recording a deterministic content-bound revision that remains resumable.
#[test]
fn record_open_allow_dirty_permits_uncommitted_bundle() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", record_open_dry_run_forge_stub());

    // The repo has history (initial commit), but the bundle files below are
    // never committed — the realistic "open a record before committing the
    // bundle" case, distinct from an empty repo with an unborn HEAD.
    let repo = init_repo_with(
        InitRepoOptions::new()
            .with_branch("main")
            .with_initial_commit(),
    );
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    // Intentionally left uncommitted (untracked working-tree files).

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());

    // Default: an uncommitted bundle is rejected.
    let blocked = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
        ],
        &opts,
    );
    assert_ne!(
        blocked.code, 0,
        "an uncommitted bundle must be rejected without --allow-dirty"
    );

    // With --allow-dirty: the open proceeds with a stable content identity.
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
            "--allow-dirty",
        ],
        &opts,
    );
    assert_eq!(
        out.code,
        0,
        "--allow-dirty must bypass the commit check; stderr: {}",
        out.stderr_text()
    );
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let preview = &parsed["payload"]["result"]["preview"];
    let source_revision = preview["source_commit"].as_str().expect("source revision");
    assert!(
        source_revision.starts_with("dirty-sha256:") && source_revision.len() == 77,
        "uncommitted snapshot must use a deterministic SHA-256 revision: {preview}"
    );
    let source_comment = preview["comments"]["source"]
        .as_str()
        .expect("source comment");
    assert!(
        source_comment.contains(&format!("- Commit: `{source_revision}`")),
        "uncommitted snapshot should expose its resumable revision:\n{source_comment}"
    );
    assert!(
        source_comment.contains("- Snapshot mode: local uncommitted Markdown"),
        "uncommitted snapshot should be labeled uncommitted:\n{source_comment}"
    );
    assert!(
        !source_comment.contains("local committed Markdown"),
        "uncommitted snapshot must not claim committed:\n{source_comment}"
    );

    fs::write(
        repo.path()
            .join("docs/plans/sample/sample-discussion-source.md"),
        "# Source\n\nChanged dirty content.\n",
    )
    .expect("change dirty source");
    let changed = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            "docs/plans/sample",
            "--allow-dirty",
        ],
        &opts,
    );
    assert_eq!(
        changed.code,
        0,
        "changed dirty preview: {}",
        changed.stderr_text()
    );
    let changed: Value = serde_json::from_str(&changed.stdout_text()).expect("changed json");
    assert_ne!(
        changed["payload"]["result"]["preview"]["source_commit"], preview["source_commit"],
        "different dirty source contents must not share a record identity"
    );
}

/// The first Execution State posted by `record open` defaults to an open fold
/// (`<details open>`) when the execution-state file carries a `## Task Ledger`,
/// so a reader sees the full plan on load while the toggle stays. Later
/// checkpoints keep the `auto` default (collapsed while in-progress).
#[test]
fn record_open_initial_state_task_ledger_defaults_to_open_fold() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", record_open_dry_run_forge_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n\n## Task Ledger\n\n| ID | Status | Task |\n| --- | --- | --- |\n| 1.1 | pending | Demo task |\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let state_comment = parsed["payload"]["result"]["preview"]["comments"]["state"]
        .as_str()
        .expect("state comment");
    assert!(
        state_comment.contains("<details open>"),
        "first Execution State should default to an open fold: {state_comment}"
    );
    assert!(
        state_comment.contains("<summary>Show task ledger</summary>"),
        "open fold must keep the toggle summary: {state_comment}"
    );
    assert!(
        state_comment.contains("| 1.1 | pending | Demo task |"),
        "ledger rows must be present inside the open fold: {state_comment}"
    );
}

/// Sprint 4 Task 4.3: exercise the v3 closeout end-to-end against a sanitized
/// agent-runtime-kit fixture. Asserts that one `record close` invocation can
/// audit the issue, verify provider PR merge evidence, render the closeout
/// comment + final dashboard, and that no v1 markers leak into the result.
#[test]
fn agent_runtime_kit_lifecycle_fixture_passes_strict_v2_closeout_end_to_end() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["operation"], "record.close");
    assert_eq!(result["mode"], "fixture");
    assert_eq!(result["dry_run"], true);

    let preview = &result["preview"];
    let closeout_body = preview["closeout_comment_body"]
        .as_str()
        .expect("closeout body present");
    // Closeout comment uses the v2 marker and carries provider-verified
    // merge_sha from the fixture PR snapshot in the hidden payload.
    assert!(
        closeout_body.starts_with("<!-- plan-issue-record:v2 role=closeout profile=tracking -->"),
        "{closeout_body}"
    );
    assert!(
        !closeout_body.contains(&format!("```{PAYLOAD_FENCE_INFO}")),
        "{closeout_body}"
    );
    assert!(
        closeout_body.contains("<!-- plan-issue-record-payload:hex:"),
        "{closeout_body}"
    );
    let audit = audit_single_comment_body(closeout_body);
    let closeout = &audit["evidence"]["closeout"]["payload"]["data"];
    assert_eq!(closeout["final_status"], "complete");
    assert_eq!(
        closeout["linked_prs"][0]["merge_sha"], "merge1111111111111111111111111111111111",
        "merge_sha must come from PR fixture, not state payload: {closeout_body}"
    );
    // Sanity: no v1 marker bleed-through.
    assert!(
        !closeout_body.contains("execute-from-tracking-issue:")
            && !closeout_body.contains("plan-tracking-issue:"),
        "v1 markers must not appear in v2 closeout body: {closeout_body}"
    );

    let final_dashboard = preview["final_dashboard"]
        .as_str()
        .expect("final dashboard present");
    assert!(
        final_dashboard.starts_with("<!-- plan-issue-record-identity:v1:hex:")
            && final_dashboard.contains("\n\n## Final Dashboard"),
        "complete state must retain identity and render Final Dashboard: {final_dashboard}"
    );
    // Durable record links derive from audit, not caller-supplied URLs.
    assert!(
        final_dashboard.contains(
            "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-source"
        ),
        "dashboard must include source URL from audit: {final_dashboard}"
    );
    assert!(
        final_dashboard
            .contains("https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-state"),
        "dashboard must include state URL from audit: {final_dashboard}"
    );
}

/// Issue sympoies/nils-cli#479: `record open --label` exposes labels in the
/// dry-run preview so downstream consumers can audit creation-time labels
/// without hitting the provider.
#[test]
fn record_open_dry_run_includes_labels_in_preview() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", record_open_dry_run_forge_stub());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: implement v2 lifecycle.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Sample Plan\n\n## Overview\n\n- Sample plan body.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo plan.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Sample Plan\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let opts = dry_run_cmd_options(stub.path()).with_cwd(repo.path());
    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "record",
            "open",
            "--bundle",
            &bundle_arg,
            "--label",
            "workflow::plan",
            "--label",
            " state::needs-triage ",
            "--label",
            "",
        ],
        &opts,
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    let labels = result["preview"]["labels"]
        .as_array()
        .expect("preview.labels array");
    let labels: Vec<&str> = labels.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        labels,
        vec!["workflow::plan", "state::needs-triage"],
        "empty/whitespace labels must be dropped and non-empty values trimmed"
    );
}

#[test]
fn record_open_recovers_store_then_error_comment_without_duplicates() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    commit_all(repo.path());

    let log_path = tmp.path().join("forge-cli.log");
    let created_issue_path = tmp.path().join("created-issue");
    let issue_body_path = tmp.path().join("issue-body.md");
    let comment_count_path = tmp.path().join("comment-count");
    let comment_store = tmp.path().join("comments");
    fs::create_dir_all(&comment_store).expect("comment store");

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let created_issue_s = created_issue_path.to_string_lossy().to_string();
    let issue_body_s = issue_body_path.to_string_lossy().to_string();
    let comment_count_s = comment_count_path.to_string_lossy().to_string();
    let comment_store_s = comment_store.to_string_lossy().to_string();
    let args = [
        "--format",
        "json",
        "--state-dir",
        state_dir_arg.as_str(),
        "--repo",
        "https://github.com/owner/repo.git",
        "record",
        "open",
        "--bundle",
        "docs/plans/sample",
        "--allow-dirty",
    ];

    let first = common::run_plan_issue_with_options(
        &args,
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                (
                    "FORGE_CLI_STUB_CREATE_URL",
                    "https://github.com/owner/repo/issues/999",
                ),
                ("FORGE_CLI_STUB_CREATED_ISSUE_FILE", &created_issue_s),
                ("FORGE_CLI_STUB_CAPTURE_CREATE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_VIEW_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_COMMENT_STORE_DIR", &comment_store_s),
                ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &comment_count_s),
                ("FORGE_CLI_STUB_STORE_THEN_FAIL_COMMENT_ON_CALL", "1"),
            ],
        )
        .with_cwd(repo.path()),
    );
    assert_eq!(
        first.code,
        0,
        "stdout={} stderr={}",
        first.stdout_text(),
        first.stderr_text()
    );
    let first_result = &first.stdout_json()["payload"]["result"];
    assert_eq!(first_result["mode"], "live");
    assert_eq!(first_result["attached"], json!(["source", "plan", "state"]));
    let marker_body = fs::read_to_string(&issue_body_path).expect("converged issue body");
    assert!(
        marker_body.starts_with("<!-- plan-issue-record-identity:v1:hex:"),
        "the converged tracker must retain its direct identity marker:\n{marker_body}"
    );
    assert_eq!(
        fs::read_dir(&comment_store).expect("comment store").count(),
        3,
        "readback must prove the stored source comment and avoid reposting it"
    );

    let second = common::run_plan_issue_with_options(
        &args,
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                (
                    "FORGE_CLI_STUB_CREATE_URL",
                    "https://github.com/owner/repo/issues/999",
                ),
                ("FORGE_CLI_STUB_CREATED_ISSUE_FILE", &created_issue_s),
                ("FORGE_CLI_STUB_CAPTURE_CREATE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_VIEW_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_COMMENT_STORE_DIR", &comment_store_s),
                ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &comment_count_s),
            ],
        )
        .with_cwd(repo.path()),
    );
    assert_eq!(
        second.code,
        0,
        "stdout={} stderr={}",
        second.stdout_text(),
        second.stderr_text()
    );
    let second_result = &second.stdout_json()["payload"]["result"];
    assert_eq!(second_result["mode"], "already-open");
    assert_eq!(second_result["attached"], json!([]));
    let provider_log = fs::read_to_string(&log_path).expect("provider log");
    assert_eq!(
        provider_log.matches("issue create").count(),
        1,
        "recovered open must not create a second tracker:\n{provider_log}"
    );
    assert_eq!(
        fs::read_to_string(&comment_count_path)
            .expect("comment count")
            .trim(),
        "3",
        "one stored-then-error source append plus two ordinary appends"
    );
    let stored_bodies = fs::read_dir(&comment_store)
        .expect("comment store")
        .map(|entry| {
            fs::read_to_string(entry.expect("comment entry").path()).expect("comment body")
        })
        .collect::<Vec<_>>();
    for role in ["source", "plan", "state"] {
        assert_eq!(
            stored_bodies
                .iter()
                .filter(|body| body.contains(&format!("role={role}")))
                .count(),
            1,
            "recovery must retain {role} exactly once"
        );
    }

    fs::write(&log_path, "").expect("reset provider log");
    let third = common::run_plan_issue_with_options(
        &args,
        live_record_options(
            stub.path(),
            &[
                ("FORGE_CLI_STUB_LOG", &log_s),
                (
                    "FORGE_CLI_STUB_CREATE_URL",
                    "https://github.com/owner/repo/issues/999",
                ),
                ("FORGE_CLI_STUB_CREATED_ISSUE_FILE", &created_issue_s),
                ("FORGE_CLI_STUB_CAPTURE_CREATE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_VIEW_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_CAPTURE_BODY_FILE", &issue_body_s),
                ("FORGE_CLI_STUB_COMMENT_STORE_DIR", &comment_store_s),
                ("FORGE_CLI_STUB_COMMENT_COUNT_FILE", &comment_count_s),
            ],
        )
        .with_cwd(repo.path()),
    );
    assert_eq!(
        third.code,
        0,
        "stdout={} stderr={}",
        third.stdout_text(),
        third.stderr_text()
    );
    let third_result = &third.stdout_json()["payload"]["result"];
    assert_eq!(third_result["mode"], "already-open");
    assert_eq!(third_result["attached"], json!([]));
    let third_log = fs::read_to_string(&log_path).expect("third provider log");
    assert!(
        !third_log.contains("issue create")
            && !third_log.contains("issue comment")
            && !third_log.contains("issue edit"),
        "a converged tracker rerun must perform provider reads only:\n{third_log}"
    );
    assert_eq!(
        fs::read_to_string(&comment_count_path)
            .expect("comment count")
            .trim(),
        "3"
    );
}

#[test]
fn record_attach_dry_run_renders_source_plan_and_state_comments() {
    use nils_test_support::git::{InitRepoOptions, git, init_repo_with};

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    let bundle = repo.path().join("docs/plans/sample");
    fs::create_dir_all(&bundle).expect("create bundle dir");
    let source = bundle.join("sample-discussion-source.md");
    let plan = bundle.join("sample-plan.md");
    let execution_state = bundle.join("sample-execution-state.md");
    fs::write(&source, "# Source\n\n- Decision: attach existing issue.\n").expect("write source");
    fs::write(
        &plan,
        "# Plan: Existing Issue Attach\n\n## Overview\n\n- Attach v2 lifecycle comments.\n\n## Read First\n\n- Primary source: docs/plans/sample/sample-discussion-source.md\n- Source type: discussion-to-implementation-doc\n- Open questions carried into execution: none\n\n## Scope\n\n- In scope:\n  - Demo attach.\n- Out of scope:\n  - none.\n\n## Assumptions\n\n1. Demo only.\n\n## Sprint 1: Demo\n\n**Goal**: Demo the attach surface.\n\n**PR grouping intent**: group\n**Execution Profile**: serial\n\n### Task 1.1: Demo task\n\n- **Location**:\n  - `docs/plans/sample/sample-plan.md`\n- **Description**: Demo task description.\n- **Dependencies**:\n  - none\n- **Complexity**: 1\n- **Acceptance criteria**:\n  - The demo task is complete.\n- **Validation**:\n  - `true`\n",
    )
    .expect("write plan");
    fs::write(
        &execution_state,
        "# Sample Execution State\n\n<!-- plan-issue-record:v2 role=state profile=tracking -->\n\n## Execution State\n\n- Status: pending\n- Target scope: Existing Issue Attach\n",
    )
    .expect("write execution state");
    git(repo.path(), &["add", "."]);
    git(
        repo.path(),
        &[
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "seed bundle",
            "--no-gpg-sign",
        ],
    );

    let bundle_arg = bundle.to_string_lossy().to_string();
    let out = nils_test_support::cmd::run_resolved(
        "plan-issue-local",
        &[
            "--format",
            "json",
            "--dry-run",
            "record",
            "attach",
            "--issue",
            "69",
            "--bundle",
            &bundle_arg,
        ],
        &nils_test_support::cmd::CmdOptions::new().with_cwd(repo.path()),
    );
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed: Value = serde_json::from_str(&out.stdout_text()).expect("json");
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["preview"]["issue_number"], 69);
    let comments = &result["preview"]["comments"];
    assert!(comments["source"].as_str().unwrap().contains("role=source"));
    assert!(comments["plan"].as_str().unwrap().contains("role=plan"));
    assert!(comments["state"].as_str().unwrap().contains("role=state"));
}

/// `record post --add-label / --remove-label` exposes the planned label
/// mutation in dry-run output and in fixture mode without touching gh.
#[test]
fn record_post_dry_run_includes_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "blocked", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::blocked",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let result = &parsed["payload"]["result"];
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["labels"]["add"][0], "state::blocked");
    assert_eq!(result["labels"]["remove"][0], "state::in-progress");
}

#[test]
fn record_close_live_refuses_busy_lock_before_evidence_fetch() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "github",
        Some("github.com"),
        "owner/repo",
        448,
        RecordProfile::Tracking,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "--repo",
            "https://github.com/owner/repo.git",
            "record",
            "close",
            "--issue",
            "448",
            "--approval",
            "https://github.com/owner/repo/issues/448#issuecomment-approval",
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "plan-issue-lifecycle-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn record_attach_live_refuses_busy_lock_before_provider_mutation() {
    use nils_test_support::git::{InitRepoOptions, init_repo_with};

    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");

    let repo = init_repo_with(InitRepoOptions::new().with_branch("main"));
    write_sample_bundle(&repo.path().join("docs/plans/sample"));
    commit_all(repo.path());

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "github",
        Some("github.com"),
        "owner/repo",
        448,
        RecordProfile::Dispatch,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "--repo",
            "https://github.com/owner/repo.git",
            "record",
            "attach",
            "--issue",
            "448",
            "--bundle",
            "docs/plans/sample",
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]).with_cwd(repo.path()),
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "plan-issue-lifecycle-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn record_repair_dashboard_live_refuses_busy_lock_before_evidence_fetch() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());
    let log_path = tmp.path().join("forge-cli.log");

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "github",
        Some("github.com"),
        "owner/repo",
        449,
        RecordProfile::Dispatch,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let log_s = log_path.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "--repo",
            "https://github.com/owner/repo.git",
            "record",
            "repair-dashboard",
            "--issue",
            "449",
        ],
        live_record_options(stub.path(), &[("FORGE_CLI_STUB_LOG", &log_s)]),
    );

    assert_eq!(out.code, 1, "stderr={}", out.stderr_text());
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "plan-issue-lifecycle-lock-busy"
    );
    assert_eq!(fs::read_to_string(log_path).unwrap_or_default(), "");
}

#[test]
fn record_post_live_refuses_when_lifecycle_lock_is_busy() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = TempDir::new().expect("state-dir");
    let stub = StubBinDir::new();
    stub.write_exe("forge-cli", common::forge_cli_stub_script());

    plan_issue::state::set_state_dir_override(Some(state_dir.path().to_path_buf()));
    let _busy_lock = plan_issue::lifecycle_lock::acquire_for_identity(
        "github",
        Some("github.com"),
        "owner/repo",
        448,
        RecordProfile::Tracking,
    )
    .expect("pre-acquire lifecycle lock");
    plan_issue::state::set_state_dir_override(None);

    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "blocked", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let state_dir_arg = state_dir.path().to_string_lossy().to_string();
    let payload_arg = payload.to_string_lossy().to_string();
    let out = common::run_plan_issue_with_options(
        &[
            "--format",
            "json",
            "--state-dir",
            &state_dir_arg,
            "--repo",
            "https://github.com/owner/repo.git",
            "record",
            "post",
            "--issue",
            "448",
            "--kind",
            "state",
            "--payload-file",
            &payload_arg,
        ],
        live_record_options(stub.path(), &[]),
    );

    assert_eq!(
        out.code,
        1,
        "stdout={} stderr={}",
        out.stdout_text(),
        out.stderr_text()
    );
    let parsed = out.stdout_json();
    assert_eq!(parsed["status"], "error");
    assert_eq!(parsed["error"]["code"], "plan-issue-lifecycle-lock-busy");
    let message = parsed["error"]["message"].as_str().expect("message");
    assert!(message.contains("issue=448"), "{message}");
    assert!(message.contains("profile=tracking"), "{message}");
}

/// `record close --add-label / --remove-label` shows the planned closeout
/// label transition in fixture preview output.
#[test]
fn record_close_fixture_includes_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "https://github.com/sympoies/agent-runtime-kit/issues/42#issuecomment-approval",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::in-progress",
    ]);
    assert_eq!(out.code, 0, "stderr: {}", out.stderr_text());
    let parsed = out.stdout_json();
    let labels = &parsed["payload"]["result"]["preview"]["labels"];
    assert_eq!(labels["add"][0], "state::closed");
    assert_eq!(labels["remove"][0], "state::in-progress");
}

/// Same label name in `--add-label` and `--remove-label` is incoherent — the
/// helper rejects it with a usage error so the live `gh issue edit` call is
/// never built.
#[test]
fn record_post_rejects_conflicting_label_mutations() {
    let tmp = TempDir::new().expect("tempdir");
    let payload = tmp.path().join("state.json");
    fs::write(
        &payload,
        json!({"status": "in-progress", "tasks": [], "prs": [], "blockers": [], "links": {}})
            .to_string(),
    )
    .expect("write payload");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "--dry-run",
        "record",
        "post",
        "--issue",
        "448",
        "--kind",
        "state",
        "--payload-file",
        payload.to_str().expect("payload path"),
        "--add-label",
        "state::needs-triage",
        "--remove-label",
        "state::needs-triage",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

#[test]
fn record_close_rejects_conflicting_label_mutations() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--remove-label",
        "state::closed",
    ]);
    assert_ne!(out.code, 0, "conflicting label mutation should fail");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("record-label-mutation-conflict"),
        "expected record-label-mutation-conflict code, got: {joined}"
    );
}

#[test]
fn record_close_rejects_multiple_terminal_state_additions() {
    let fixture = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout").to_path_buf();
    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
        "--add-label",
        "state::closed",
        "--add-label",
        "state::ready",
    ]);
    assert_ne!(out.code, 0, "multiple terminal states must fail");
    assert_eq!(
        out.stdout_json()["error"]["code"],
        "record-close-state-label-conflict"
    );
}

/// Sprint 4 Task 4.3: same fixture, but force the strict gate to fail by
/// flipping the PR snapshot to unmerged. Verifies the gate code surfaces.
#[test]
fn agent_runtime_kit_lifecycle_fixture_blocks_when_pr_unmerged() {
    let src = Path::new("tests/fixtures/lifecycle/agent-runtime-kit-closeout");
    let tmp = TempDir::new().expect("tmp");
    let fixture = tmp.path().join("fixture");
    fs::create_dir_all(fixture.join("prs")).expect("create fixture dirs");
    fs::copy(src.join("issue-body.md"), fixture.join("issue-body.md")).expect("copy body");
    fs::copy(src.join("comments.json"), fixture.join("comments.json")).expect("copy comments");
    // Replace the PR snapshot with an open PR so the strict gate fails.
    fs::write(
        fixture.join("prs/sympoies__agent-runtime-kit__1.json"),
        serde_json::to_string(&json!({
            "state": "OPEN",
            "mergeCommit": null,
            "statusCheckRollup": {"state": "pending"},
            "url": "https://github.com/sympoies/agent-runtime-kit/pull/1"
        }))
        .expect("pr json"),
    )
    .expect("write open pr fixture");

    let out = common::run_plan_issue_local(&[
        "--format",
        "json",
        "record",
        "close",
        "--issue",
        "42",
        "--linked-pr",
        "sympoies/agent-runtime-kit#1",
        "--approval",
        "ok",
        "--fixture",
        fixture.to_str().expect("fixture path"),
    ]);

    assert_ne!(out.code, 0, "unmerged PR should block strict closeout");
    let joined = format!("{}\n{}", out.stderr_text(), out.stdout_text());
    assert!(
        joined.contains("linked-pr-not-merged"),
        "expected linked-pr-not-merged code, got: {joined}"
    );
}
