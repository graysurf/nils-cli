//! Integration coverage for `evidence migrate` prepare (dry-run) and apply.
//!
//! Builds a throwaway agent-out tree + an initialized archive clone +
//! `config/hosts.yaml`, then drives `migrate::prepare`/`apply`. The apply path
//! shells out to `semantic-commit` and `git push`; both are stubbed (the stub
//! commits locally; the archive is given a bare push remote).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use evidence::migrate::{self, DispatchArgs};
use nils_common::cli_contract::OutputFormat;

struct Scenario {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    source_out: PathBuf,
    archive: PathBuf,
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "tester")
        .env("GIT_AUTHOR_EMAIL", "tester@example.com")
        .env("GIT_COMMITTER_NAME", "tester")
        .env("GIT_COMMITTER_EMAIL", "tester@example.com")
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

fn record_json(
    skill: &str,
    status: &str,
    intent: &str,
    with_producer: bool,
    secret: bool,
) -> String {
    let producer = if with_producer {
        r#""producer": { "tool": "skill-usage", "nils_cli_version": "1.4.0" },"#
    } else {
        ""
    };
    let summary = if secret {
        "done auth: ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa next"
    } else {
        "done"
    };
    format!(
        r#"{{
            "schema": "skill-usage.record.v1",
            {producer}
            "skill": "{skill}",
            "started_at": "2026-06-14T10:00:00Z",
            "ended_at": "2026-06-14T10:30:00Z",
            "cwd": "/Users/tester/Project/kit",
            "trigger": "user_explicit",
            "intent": "{intent}",
            "inputs": {{ "user_request_summary": "x", "referenced_files": [], "external_sources": [] }},
            "outcome": {{ "status": "{status}", "summary": "{summary}" }},
            "artifacts": [],
            "linked_records": [],
            "validation": [
                {{ "command": "cargo test", "status": "pass", "summary": "" }}
            ],
            "failures": []
        }}"#
    )
}

fn write_record(source_out: &Path, project: &str, ts: &str, body: &str) {
    let dir = source_out.join(project).join(format!("{ts}-skill-usage"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("skill-usage.record.json"), body).unwrap();
}

/// Build a record body whose `linked_records` is the given JSON array literal,
/// started on `started` (`YYYY-MM-DDThh:mm:ssZ`).
fn record_json_with_links(skill: &str, started: &str, links_json: &str) -> String {
    format!(
        r#"{{
            "schema": "skill-usage.record.v1",
            "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.4.0" }},
            "skill": "{skill}",
            "started_at": "{started}",
            "ended_at": "{started}",
            "cwd": "/Users/tester/Project/kit",
            "trigger": "user_explicit",
            "intent": "intent",
            "inputs": {{ "user_request_summary": "x", "referenced_files": [], "external_sources": [] }},
            "outcome": {{ "status": "pass", "summary": "done" }},
            "artifacts": [],
            "linked_records": {links_json},
            "validation": [],
            "failures": []
        }}"#
    )
}

/// Write a record plus a sibling child file inside the same run dir, returning
/// the run dir.
fn write_record_with_child(
    source_out: &Path,
    project: &str,
    ts: &str,
    body: &str,
    child_name: &str,
    child_body: &str,
) -> PathBuf {
    let dir = source_out.join(project).join(format!("{ts}-skill-usage"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("skill-usage.record.json"), body).unwrap();
    fs::write(dir.join(child_name), child_body).unwrap();
    dir
}

fn build_scenario() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let source_out = root.join("out").join("projects");
    let archive = root.join("archive");
    fs::create_dir_all(&source_out).unwrap();
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();

    // hosts.yaml
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  github.com:\n    class: personal\n    primary_identity: graysurf\n",
    )
    .unwrap();

    // Two records under graysurf__kit, one with a secret in the summary, one
    // without a producer block.
    write_record(
        &source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json("deliver-pr", "pass", "deliver a PR", true, true),
    );
    write_record(
        &source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json("code-review", "fail", "review the diff", false, false),
    );

    // Initialize the archive as a git repo with a clean commit.
    git(&archive, &["init", "-q", "-b", "main"]);
    git(&archive, &["add", "-A"]);
    git(
        &archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );

    Scenario {
        _tmp: tmp,
        root,
        source_out,
        archive,
    }
}

/// Like `build_scenario` but writes NO records, so a test can author its own.
fn build_empty_scenario() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let source_out = root.join("out").join("projects");
    let archive = root.join("archive");
    fs::create_dir_all(&source_out).unwrap();
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  github.com:\n    class: personal\n    primary_identity: graysurf\n",
    )
    .unwrap();
    git(&archive, &["init", "-q", "-b", "main"]);
    git(&archive, &["add", "-A"]);
    git(
        &archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    Scenario {
        _tmp: tmp,
        root,
        source_out,
        archive,
    }
}

/// Like `build_empty_scenario` but with a multi-host `config/hosts.yaml` so the
/// agent-out `<owner__repo>` slug cannot pin a host on its own (F8). A test can
/// then exercise the cwd-fallback / `--host` override / blocked paths.
fn build_multi_host_empty_scenario() -> Scenario {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let source_out = root.join("out").join("projects");
    let archive = root.join("archive");
    fs::create_dir_all(&source_out).unwrap();
    fs::create_dir_all(archive.join("config")).unwrap();
    fs::create_dir_all(archive.join("evidence")).unwrap();
    fs::write(
        archive.join("config").join("hosts.yaml"),
        "version: 1\nhosts:\n  gitlab.example.com:\n    class: employer\n    employer: ExampleCo\n  github.com:\n    class: personal\n    primary_identity: graysurf\n",
    )
    .unwrap();
    git(&archive, &["init", "-q", "-b", "main"]);
    git(&archive, &["add", "-A"]);
    git(
        &archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    Scenario {
        _tmp: tmp,
        root,
        source_out,
        archive,
    }
}

/// Build a record body with an explicit `cwd`, started on `started`.
fn record_json_with_cwd(skill: &str, started: &str, cwd: &str) -> String {
    format!(
        r#"{{
            "schema": "skill-usage.record.v1",
            "producer": {{ "tool": "skill-usage", "nils_cli_version": "1.4.0" }},
            "skill": "{skill}",
            "started_at": "{started}",
            "ended_at": "{started}",
            "cwd": "{cwd}",
            "trigger": "user_explicit",
            "intent": "intent",
            "inputs": {{ "user_request_summary": "x", "referenced_files": [], "external_sources": [] }},
            "outcome": {{ "status": "pass", "summary": "done" }},
            "artifacts": [],
            "linked_records": [],
            "validation": [],
            "failures": []
        }}"#
    )
}

/// Create a real git checkout at `path` whose `origin` points at `<host>/<org>/<repo>`,
/// returning the absolute checkout path. Used to give a record a resolvable
/// `cwd -> origin` identity.
fn make_git_checkout(root: &Path, name: &str, origin_url: &str) -> PathBuf {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["remote", "add", "origin", origin_url]);
    dir
}

fn dry_run_args(s: &Scenario) -> DispatchArgs {
    DispatchArgs {
        source_out: Some(s.source_out.clone()),
        archive: Some(s.archive.clone()),
        hosts: None,
        repo: None,
        skill: None,
        since: None,
        until: None,
        promotion_only: false,
        apply: false,
        host: None,
        working_repo_roots: Vec::new(),
        format: OutputFormat::Json,
    }
}

#[test]
fn migrate_skips_and_reports_unresolvable_records_without_aborting() {
    // Core fix (A): under a multi-host hosts.yaml a slug-only record whose cwd
    // cannot be resolved is UNRESOLVABLE (F8 refuses to guess a host). It must
    // be SKIPPED and REPORTED in `blocked`, never abort the whole batch. The
    // resolvable record (a cwd pointing at a real git checkout) still rolls up.
    let s = build_multi_host_empty_scenario();

    // Resolvable record: cwd is a real git checkout on github.com.
    let checkout = make_git_checkout(&s.root, "live-checkout", "git@github.com:graysurf/kit.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    // Unresolvable record: empty cwd, slug carries no host -> blocked under a
    // multi-host config.
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json_with_cwd("code-review", "2026-06-14T11:00:00Z", ""),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed, not abort");
    assert_eq!(report.scanned, 2);
    assert_eq!(report.eligible, 1, "the resolvable record rolls up");
    assert_eq!(
        report.blocked.len(),
        1,
        "the unresolvable record is blocked"
    );
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(report.records[0].rollup.repo.repo, "kit");
    // The blocked entry names the record and a reason.
    let blocked = &report.blocked[0];
    assert!(
        blocked.record_path.contains("20260614-110000"),
        "blocked entry should name the skipped record: {}",
        blocked.record_path
    );
    assert!(
        !blocked.reason.is_empty(),
        "blocked entry must carry a reason"
    );
}

#[test]
fn migrate_blocks_resolved_host_absent_from_hosts_yaml() {
    // Core safety fix: a record whose host RESOLVES (via cwd -> origin) but is
    // ABSENT from config/hosts.yaml must be SKIPPED and REPORTED in `blocked`,
    // not silently archived as "unknown personal". The archive can only hold
    // records for hosts the operator has explicitly classified.
    let s = build_multi_host_empty_scenario(); // hosts: gitlab.example.com, github.com

    // Resolvable + classified (github.com) -> rolls up.
    let classified = make_git_checkout(&s.root, "classified", "git@github.com:graysurf/kit.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &classified.to_string_lossy().replace('\\', "/"),
        ),
    );

    // Resolvable but UNCLASSIFIED: cwd resolves to gitlab.com, which is absent
    // from this archive's hosts.yaml -> must be blocked, not written.
    let unclassified =
        make_git_checkout(&s.root, "unclassified", "git@gitlab.com:someone/proj.git");
    write_record(
        &s.source_out,
        "someone__proj",
        "20260614-110000",
        &record_json_with_cwd(
            "code-review",
            "2026-06-14T11:00:00Z",
            &unclassified.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed, not abort");
    assert_eq!(report.scanned, 2);
    assert_eq!(report.eligible, 1, "only the classified record rolls up");
    assert_eq!(report.records.len(), 1);
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(
        report.blocked.len(),
        1,
        "the unclassified-host record is blocked, not written"
    );
    let blocked = &report.blocked[0];
    assert!(
        blocked.record_path.contains("20260614-110000"),
        "blocked entry should name the unclassified record: {}",
        blocked.record_path
    );
    assert!(
        blocked.reason.contains("not classified") && blocked.reason.contains("gitlab.com"),
        "blocked reason should explain the missing classification: {}",
        blocked.reason
    );
}

#[test]
fn migrate_rescues_unresolvable_identity_via_working_repo_roots() {
    // N2 part 1: a record whose recorded cwd no longer exists (e.g. a removed
    // agent worktree) is normally UNRESOLVABLE under a multi-host config. When a
    // configured working_repo_roots entry holds a matching local checkout, the
    // host is recovered from that checkout's origin and the record rolls up.
    let s = build_multi_host_empty_scenario(); // gitlab.example.com, github.com

    // A local checkout mirror under a working-repo root; its origin pins the host.
    let roots = s.root.join("mirror");
    make_git_checkout(
        &roots.join("graysurf"),
        "kit",
        "git@github.com:graysurf/kit.git",
    );

    // Record whose cwd does not exist -> derive_repo_identity fails (Unresolvable).
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &s.root
                .join("gone-worktree")
                .to_string_lossy()
                .replace('\\', "/"),
        ),
    );

    let mut args = dry_run_args(&s);
    args.working_repo_roots = vec![roots.clone()];
    let report = migrate::prepare(&args).expect("dry-run must succeed");
    assert_eq!(report.eligible, 1, "the record is rescued and rolls up");
    assert_eq!(
        report.blocked.len(),
        0,
        "rescued via working_repo_roots, not blocked"
    );
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(report.records[0].rollup.repo.org, "graysurf");
    assert_eq!(report.records[0].rollup.repo.repo, "kit");
}

#[test]
fn migrate_single_host_blocks_record_whose_cwd_resolves_to_other_host() {
    // #872 part 1: under a SINGLE-host config the slug used to map straight to
    // the sole host without consulting the record's own cwd. A record whose
    // cwd -> origin resolves to a DIFFERENT, unclassified host must instead be
    // blocked, not silently archived under the sole configured host.
    let s = build_empty_scenario(); // single host: github.com

    let elsewhere = make_git_checkout(&s.root, "elsewhere", "git@gitlab.com:someone/proj.git");
    write_record(
        &s.source_out,
        "someone__proj",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &elsewhere.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a record whose cwd resolves to an unclassified host must not roll up"
    );
    assert_eq!(report.blocked.len(), 1, "it is blocked, not archived");
    assert!(
        report.blocked[0].reason.contains("not classified")
            && report.blocked[0].reason.contains("gitlab.com"),
        "blocked reason should name the unclassified resolved host: {}",
        report.blocked[0].reason
    );
}

#[test]
fn migrate_rescues_nested_gitlab_checkout_via_working_repo_roots() {
    // #872 part 2: a nested GitLab group has a full origin org such as
    // `acme/platform/backend`, but the agent-out slug keeps only the last owner
    // segment (`backend__svc`). The working_repo_roots rescue must still find
    // the checkout (mirrored at its full origin path) by matching the normalized
    // slug, and preserve the FULL origin org/repo in the rollup.
    let s = build_multi_host_empty_scenario(); // gitlab.example.com (employer), github.com

    let roots = s.root.join("mirror");
    make_git_checkout(
        &roots.join("acme/platform/backend"),
        "svc",
        "git@gitlab.example.com:acme/platform/backend/svc.git",
    );

    // Record whose recorded cwd is gone -> Unresolvable -> rescue path.
    write_record(
        &s.source_out,
        "backend__svc",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &s.root.join("gone").to_string_lossy().replace('\\', "/"),
        ),
    );

    let mut args = dry_run_args(&s);
    args.working_repo_roots = vec![roots.clone()];
    let report = migrate::prepare(&args).expect("dry-run must succeed");
    assert_eq!(report.eligible, 1, "the nested checkout is rescued");
    assert_eq!(report.blocked.len(), 0, "rescued, not blocked");
    assert_eq!(report.records[0].rollup.repo.host, "gitlab.example.com");
    assert_eq!(
        report.records[0].rollup.repo.org, "acme/platform/backend",
        "the full origin org is preserved, not the truncated slug owner"
    );
    assert_eq!(report.records[0].rollup.repo.repo, "svc");
}

#[test]
fn migrate_single_host_uses_slug_when_cwd_repointed_to_other_repo() {
    // #876 part 1: under a single-host config a record's recorded cwd may have
    // been repointed or reused for a DIFFERENT checkout on the same host. The
    // cwd identity then disagrees with the agent-out slug; trusting it would
    // archive the record under the wrong (org, repo). The slug is authoritative
    // for the repo, so the record must roll up under the slug + sole host, not
    // the cwd's repo.
    let s = build_empty_scenario(); // single host: github.com

    // cwd resolves, but to a different repo than the record's slug.
    let reused = make_git_checkout(&s.root, "reused", "git@github.com:other/reused.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &reused.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 1,
        "the record rolls up under its slug identity"
    );
    assert_eq!(
        report.blocked.len(),
        0,
        "recovered from the slug, not blocked"
    );
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(
        report.records[0].rollup.repo.org, "graysurf",
        "org must come from the authoritative slug, not the repointed cwd"
    );
    assert_eq!(report.records[0].rollup.repo.repo, "kit");
}

#[test]
fn migrate_blocks_ambiguous_rescue_slug() {
    // #876 part 2: when two checkouts under working_repo_roots normalize to the
    // SAME agent-out slug but resolve to DIFFERENT identities, the slug is
    // ambiguous. With the record's own cwd gone there is no signal to pick one,
    // so the record must stay BLOCKED rather than be rescued to whichever
    // checkout was walked first (which could mis-archive employer / personal
    // evidence by directory order).
    let s = build_multi_host_empty_scenario(); // gitlab.example.com, github.com

    let roots = s.root.join("mirror");
    // Both checkouts normalize to `teamx__widget` but resolve to different
    // identities (different host and org).
    make_git_checkout(
        &roots.join("a"),
        "widget",
        "git@github.com:teamx/widget.git",
    );
    make_git_checkout(
        &roots.join("b/group"),
        "widget",
        "git@gitlab.example.com:group/teamx/widget.git",
    );

    // Record whose recorded cwd is gone -> Unresolvable -> rescue path.
    write_record(
        &s.source_out,
        "teamx__widget",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &s.root.join("gone").to_string_lossy().replace('\\', "/"),
        ),
    );

    let mut args = dry_run_args(&s);
    args.working_repo_roots = vec![roots.clone()];
    let report = migrate::prepare(&args).expect("dry-run must succeed");
    assert_eq!(report.eligible, 0, "an ambiguous slug must not be rescued");
    assert_eq!(
        report.blocked.len(),
        1,
        "the record is blocked, not mis-archived to the first-walked checkout"
    );
    assert!(
        report.blocked[0]
            .reason
            .to_lowercase()
            .contains("ambiguous"),
        "blocked reason should explain the ambiguity: {}",
        report.blocked[0].reason
    );
}

#[test]
fn migrate_rescue_ignores_checkout_whose_slug_does_not_match() {
    // The rescue matches strictly by normalized slug: a decoy checkout under
    // working_repo_roots that does NOT match the record's slug must be ignored,
    // so the record stays blocked rather than being mis-attributed to the wrong
    // repo's host.
    let s = build_multi_host_empty_scenario();

    let roots = s.root.join("mirror");
    make_git_checkout(
        &roots.join("other"),
        "thing",
        "git@github.com:other/thing.git",
    );

    write_record(
        &s.source_out,
        "backend__svc",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &s.root.join("gone").to_string_lossy().replace('\\', "/"),
        ),
    );

    let mut args = dry_run_args(&s);
    args.working_repo_roots = vec![roots.clone()];
    let report = migrate::prepare(&args).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a non-matching checkout must not rescue the record"
    );
    assert_eq!(report.blocked.len(), 1, "the record stays blocked");
}

#[test]
fn migrate_single_host_local_fallback_slug_with_changed_origin_uses_placeholder() {
    // #879 follow-up: agent-out's local-fallback slug (`local__<base>-<hash>`,
    // emitted when a repo had NO resolvable origin) is INDISTINGUISHABLE by
    // string from a real provider repo owned by `local`, so there is no sound
    // way to "recover" it to a checkout that later gained a different origin (a
    // repointed cwd looks identical). The matcher therefore treats it like any
    // other slug: a cwd whose identity does not normalize to the slug does not
    // match, so under single-host the record archives under the placeholder slug
    // + sole host rather than trusting the (possibly repointed) cwd.
    let s = build_empty_scenario(); // single host: github.com

    let checkout = make_git_checkout(&s.root, "nils-cli", "git@github.com:sympoies/nils-cli.git");
    write_record(
        &s.source_out,
        "local__nils-cli-deadbeef",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 1,
        "single-host falls back to the slug + sole host"
    );
    assert_eq!(report.blocked.len(), 0);
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(
        report.records[0].rollup.repo.org, "local",
        "the placeholder slug owner is used, NOT the possibly-repointed cwd origin"
    );
    assert_eq!(report.records[0].rollup.repo.repo, "nils-cli-deadbeef");
}

#[test]
fn migrate_multi_host_accepts_manual_slug_with_case_only_origin_difference() {
    // #879 follow-up (r495): a manual dir `acme__my_repo` whose origin
    // differs only by CASE (`Acme/my_repo`) must still match — case is folded,
    // the underscore is not. (The earlier raw-only check wrongly blocked this.)
    let s = build_multi_host_empty_scenario(); // gitlab.example.com, github.com

    let checkout = make_git_checkout(&s.root, "my_repo", "git@github.com:Acme/my_repo.git");
    write_record(
        &s.source_out,
        "acme__my_repo",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 1,
        "case-only origin difference still matches the manual slug"
    );
    assert_eq!(report.blocked.len(), 0);
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
}

#[test]
fn migrate_multi_host_blocks_real_local_repo_repointed_to_nonlocal_owner() {
    // #879 follow-up (r499): a real provider repo shaped like a fallback
    // (`local__widget-deadbeef`) repointed to a DIFFERENT owner (`other/widget`)
    // must be blocked. The earlier `is_local_fallback && org != local` special
    // case wrongly TRUSTED any non-`local` repoint; uniform matching rejects it.
    let s = build_multi_host_empty_scenario();

    let reused = make_git_checkout(&s.root, "widget", "git@github.com:other/widget.git");
    write_record(
        &s.source_out,
        "local__widget-deadbeef",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &reused.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a repoint to a different owner must not be trusted"
    );
    assert_eq!(report.blocked.len(), 1, "the mismatched cwd is blocked");
}

#[test]
fn migrate_multi_host_accepts_manual_underscore_slug_matching_cwd() {
    // #877 follow-up part 2: a manual slug whose repo half keeps an
    // underscore (`acme__my_repo`) is compared against a cwd origin
    // (`acme/my_repo`) that the slug rule normalizes to `acme__my-repo`.
    // Comparing the raw directory name against the normalized cwd slug would
    // wrongly reject a cwd that points at the SAME repo. Both sides must be
    // normalized through the same rule, so the matching cwd is trusted (and,
    // under multi-host, supplies the otherwise-unknowable host).
    let s = build_multi_host_empty_scenario(); // gitlab.example.com, github.com

    let checkout = make_git_checkout(&s.root, "my_repo", "git@github.com:acme/my_repo.git");
    write_record(
        &s.source_out,
        "acme__my_repo",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 1,
        "the underscore slug matches its cwd after normalization"
    );
    assert_eq!(
        report.blocked.len(),
        0,
        "a matching cwd must not be blocked over slug formatting"
    );
    assert_eq!(report.records[0].rollup.repo.host, "github.com");
    assert_eq!(report.records[0].rollup.repo.org, "acme");
    assert_eq!(report.records[0].rollup.repo.repo, "my_repo");
}

#[test]
fn migrate_host_override_blocks_resolvable_mismatched_cwd() {
    // #877 follow-up part 3: `--host` is a GLOBAL operator override meant to
    // rescue slug-only records whose cwd is GONE. A record whose cwd still
    // RESOLVES but to a DIFFERENT checkout (repointed/reused) is not slug-only;
    // slapping the override onto its slug would mis-attribute it. Such a record
    // must be blocked as unresolvable, not archived under the override host.
    let s = build_multi_host_empty_scenario(); // gitlab.example.com (employer), github.com

    // cwd resolves, but to a different repo than the record's slug.
    let reused = make_git_checkout(&s.root, "reused", "git@github.com:other/reused.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &reused.to_string_lossy().replace('\\', "/"),
        ),
    );

    let mut args = dry_run_args(&s);
    args.host = Some("gitlab.example.com".to_string());
    let report = migrate::prepare(&args).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a resolvable-but-mismatched cwd must not be archived under --host"
    );
    assert_eq!(
        report.blocked.len(),
        1,
        "the record is blocked as unresolvable"
    );
}

#[test]
fn migrate_multi_host_blocks_manual_underscore_slug_repointed_to_hyphen_repo() {
    // #878 follow-up A: normalizing BOTH sides over-collapsed `_` and `-`, so a
    // manual slug `acme__my_repo` matched a cwd repointed to the UNRELATED
    // repo `acme/my-repo` (provider repo names distinguish `_` from `-`). The
    // mismatched cwd must be rejected, not trusted: under multi-host the record
    // is blocked rather than mis-archived under the reused checkout.
    let s = build_multi_host_empty_scenario(); // gitlab.example.com, github.com

    // cwd resolves to acme/my-repo (hyphen) — a different repo than acme__my_repo.
    let reused = make_git_checkout(&s.root, "my-repo", "git@github.com:acme/my-repo.git");
    write_record(
        &s.source_out,
        "acme__my_repo",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &reused.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a cwd at acme/my-repo must not match the manual slug acme__my_repo"
    );
    assert_eq!(report.blocked.len(), 1, "the mismatched cwd is blocked");
}

#[test]
fn migrate_multi_host_blocks_real_local_org_repo_repointed_cwd() {
    // #878 follow-up B: a legitimate provider repo whose slug is shaped like a
    // local fallback (`local__widget-deadbeef`: owner `local`, repo ending in
    // `-<8 hex>`) must still get mismatch protection. The hash-suffix shape alone
    // does not make it a fallback, so a cwd repointed to a DIFFERENT `local/...`
    // repo must be rejected, not trusted.
    let s = build_multi_host_empty_scenario();

    let reused = make_git_checkout(
        &s.root,
        "other-widget",
        "git@github.com:local/other-widget.git",
    );
    write_record(
        &s.source_out,
        "local__widget-deadbeef",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &reused.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(
        report.eligible, 0,
        "a real local/... repo must not accept an unrelated repointed cwd"
    );
    assert_eq!(report.blocked.len(), 1, "the mismatched cwd is blocked");
}

#[test]
fn migrate_single_host_real_local_org_repo_matches_own_cwd() {
    // Guard for #878 follow-up B: a real `local/widget-deadbeef` repo whose cwd
    // still points at itself is trusted (exact (org, repo) match), so tightening
    // the fallback heuristic does not regress legitimate `local/...` repos.
    let s = build_empty_scenario(); // single host: github.com

    let checkout = make_git_checkout(
        &s.root,
        "widget-deadbeef",
        "git@github.com:local/widget-deadbeef.git",
    );
    write_record(
        &s.source_out,
        "local__widget-deadbeef",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed");
    assert_eq!(report.eligible, 1, "the matching cwd is trusted");
    assert_eq!(report.blocked.len(), 0, "not blocked");
    assert_eq!(report.records[0].rollup.repo.org, "local");
    assert_eq!(report.records[0].rollup.repo.repo, "widget-deadbeef");
}

#[test]
fn migrate_skips_and_reports_unparseable_records_without_aborting() {
    // Core fix (A), parse path: a malformed / truncated skill-usage.record.json
    // (trailing characters after the JSON object) must be SKIPPED and REPORTED
    // in `blocked`, never abort the whole batch.
    let s = build_multi_host_empty_scenario();

    // Parseable, resolvable record: cwd is a real git checkout on github.com.
    let checkout = make_git_checkout(&s.root, "live-checkout", "git@github.com:graysurf/kit.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );

    // Malformed record: a valid object followed by trailing characters.
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-120000",
        &format!(
            "{}\nTRAILING GARBAGE\n",
            record_json_with_cwd("code-review", "2026-06-14T12:00:00Z", "")
        ),
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run must succeed, not abort");
    assert_eq!(report.scanned, 2);
    assert_eq!(report.eligible, 1, "the parseable record rolls up");
    assert_eq!(report.blocked.len(), 1, "the malformed record is blocked");
    let blocked = &report.blocked[0];
    assert!(
        blocked.record_path.contains("20260614-120000"),
        "blocked entry should name the malformed record: {}",
        blocked.record_path
    );
    assert!(
        blocked.reason.contains("parse failed"),
        "blocked reason should identify the parse failure: {}",
        blocked.reason
    );
}

#[test]
fn migrate_all_blocked_is_successful_no_op() {
    // A run where every record is blocked is a success (no-op), reporting them.
    let s = build_multi_host_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json_with_cwd("code-review", "2026-06-14T11:00:00Z", ""),
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("all-blocked is not an error");
    assert_eq!(report.eligible, 0);
    assert_eq!(report.blocked.len(), 1);
}

#[test]
fn migrate_host_override_resolves_slug_only_record() {
    // Fix (B): `--host github.com` lets a slug-only record (empty cwd) resolve
    // to github.com/<org>/<repo>, bypassing the multi-host cwd ambiguity.
    let s = build_multi_host_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json_with_cwd("code-review", "2026-06-14T11:00:00Z", ""),
    );
    let mut args = dry_run_args(&s);
    args.host = Some("github.com".to_string());
    let report = migrate::prepare(&args).expect("prepare with host override");
    assert_eq!(report.eligible, 1, "the slug record now resolves");
    assert_eq!(report.blocked.len(), 0);
    let id = &report.records[0].rollup.repo;
    assert_eq!(id.host, "github.com");
    assert_eq!(id.org, "graysurf");
    assert_eq!(id.repo, "kit");
}

#[test]
fn migrate_host_override_rejects_host_absent_from_config() {
    // Fix (B): `--host nope.example` is not present in hosts.yaml; the record
    // is blocked with a clear reason rather than silently archived.
    let s = build_multi_host_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json_with_cwd("code-review", "2026-06-14T11:00:00Z", ""),
    );
    let mut args = dry_run_args(&s);
    args.host = Some("nope.example".to_string());
    let report = migrate::prepare(&args).expect("prepare must not abort on a bad host");
    assert_eq!(report.eligible, 0);
    assert_eq!(report.blocked.len(), 1);
    assert!(
        report.blocked[0].reason.contains("nope.example"),
        "reason should name the absent host: {}",
        report.blocked[0].reason
    );
}

#[test]
fn host_override_does_not_clobber_a_resolvable_cwd() {
    // `--host` is global and meant to rescue slug-only records whose cwd cannot
    // resolve. It must NOT override the authoritative `cwd -> origin` identity
    // of a record that DOES resolve (even to a different configured host) —
    // otherwise rescuing one employer record mis-attributes a personal one.
    let s = build_multi_host_empty_scenario();
    let checkout = make_git_checkout(&s.root, "live-checkout", "git@github.com:graysurf/kit.git");
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json_with_cwd(
            "deliver-pr",
            "2026-06-14T10:00:00Z",
            &checkout.to_string_lossy().replace('\\', "/"),
        ),
    );
    // Operator passes --host for a different (employer) host to rescue some
    // other blocked slug-only record in the same batch.
    let mut args = dry_run_args(&s);
    args.host = Some("gitlab.example.com".to_string());
    let report = migrate::prepare(&args).expect("prepare");
    assert_eq!(report.eligible, 1);
    assert_eq!(report.blocked.len(), 0);
    assert_eq!(
        report.records[0].rollup.repo.host, "github.com",
        "a resolvable cwd->origin must win over the global --host override"
    );
}

#[test]
fn dry_run_derives_rollups_and_scrubs_without_writing() {
    let s = build_scenario();
    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run prepare");

    assert_eq!(report.scanned, 2);
    assert_eq!(report.eligible, 2);
    assert_eq!(report.skipped, 0);

    // Repo identity comes from the agent-out dir name graysurf__kit.
    for rec in &report.records {
        assert_eq!(rec.rollup.repo.host, "github.com");
        assert_eq!(rec.rollup.repo.org, "graysurf");
        assert_eq!(rec.rollup.repo.repo, "kit");
        assert!(rec.rollup.id.starts_with("20260614T100000Z-"));
        assert!(rec.rollup.source_digest.starts_with("sha256:"));
        assert_eq!(rec.rollup.counts.validation, 1);
    }

    // The record carrying a gh token fires the scrub.
    let scrubbed = report
        .records
        .iter()
        .find(|r| r.rollup.skill == "deliver-pr")
        .unwrap();
    assert!(scrubbed.scrub.total_matches >= 1);
    assert!(
        scrubbed
            .scrub
            .patterns_triggered
            .iter()
            .any(|p| p == "github-token")
    );
    assert!(!scrubbed.rollup.outcome.summary.contains("ghp_"));

    // The record without a producer is synthesized + warned.
    let no_producer = report
        .records
        .iter()
        .find(|r| r.rollup.skill == "code-review")
        .unwrap();
    assert_eq!(no_producer.rollup.producer.tool, "skill-usage");
    assert!(no_producer.rollup.producer.nils_cli_version.is_none());
    assert!(no_producer.warnings.iter().any(|w| w.contains("producer")));

    // Nothing was written to the archive.
    let written: Vec<_> = walk(&s.archive.join("evidence"))
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("skill-usage.rollup.json"))
        .collect();
    assert!(written.is_empty(), "dry-run must not write rollups");
}

#[test]
fn migrate_home_relativizes_skill_path_no_machine_leak() {
    // Regression: skill_slug() slugs the FULL skill path, so an absolute skill
    // path (e.g. /Users/<user>/.../SKILL.md) leaked the machine home both into
    // the committed rollup `skill` field AND the derived rollup `id` / directory
    // name (as `users-<user>-...`). A skill path under a foreign home must never
    // commit a raw machine path; it redacts. (The under-$HOME `~/...`
    // relativization branch is covered by the scrub_skill_path unit test, which
    // controls $HOME; here we assert the no-leak invariant independent of the
    // test runner's home.)
    let s = build_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json(
            "/Users/someoneelse/Project/kit/build/codex/plugins/pr/skills/deliver-pr/SKILL.md",
            "pass",
            "deliver a PR",
            true,
            false,
        ),
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("dry-run prepare");
    assert_eq!(report.eligible, 1);
    let rec = &report.records[0];
    assert!(
        !rec.rollup.skill.contains("/Users/"),
        "rollup.skill must not leak a raw machine path, got `{}`",
        rec.rollup.skill
    );
    assert!(
        !rec.rollup.id.contains("users-"),
        "rollup.id must not leak a machine home slug, got `{}`",
        rec.rollup.id
    );
}

#[test]
fn dry_run_dedups_already_archived_via_catalog_source_digest() {
    let s = build_scenario();
    // First compute the digest the prepare would assign.
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let digest = report.records[0].rollup.source_digest.clone();

    // Seed catalog.json with that digest.
    let catalog = format!(
        r#"{{ "schema_version": "evidence.catalog.v1", "records": [ {{ "source_digest": "{digest}" }} ] }}"#
    );
    fs::write(s.archive.join("catalog.json"), catalog).unwrap();

    let report2 = migrate::prepare(&dry_run_args(&s)).expect("prepare again");
    assert_eq!(report2.skipped, 1, "the seeded digest is deduped");
    assert_eq!(report2.eligible, 1);
    assert!(
        report2
            .already_archived
            .iter()
            .any(|a| a.source_digest == digest)
    );
}

#[test]
fn filters_by_skill_and_since() {
    let s = build_scenario();
    let mut args = dry_run_args(&s);
    args.skill = Some("deliver".to_string());
    let report = migrate::prepare(&args).expect("prepare");
    assert_eq!(report.eligible, 1);
    assert_eq!(report.records[0].rollup.skill, "deliver-pr");
}

#[test]
#[cfg(unix)]
fn apply_writes_one_batch_commit() {
    let s = build_scenario();
    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);

    let commits_before = git_count_commits(&s.archive);

    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });

    assert_eq!(applied.archived, 2);
    assert_eq!(applied.skipped, 0);
    assert!(!applied.archive_commit.is_empty());

    // Exactly ONE batch commit was created covering both records.
    let commits_after = git_count_commits(&s.archive);
    assert_eq!(
        commits_after - commits_before,
        1,
        "migrate apply must produce a single batch commit"
    );

    // Rollups + metadata + catalog were written.
    let rollups: Vec<_> = walk(&s.archive.join("evidence"))
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("skill-usage.rollup.json"))
        .collect();
    assert_eq!(rollups.len(), 2);
    assert!(s.archive.join("catalog.json").is_file());

    // metadata.yaml carries metadata_version: 1.
    let metas: Vec<_> = walk(&s.archive.join("evidence"))
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("metadata.yaml"))
        .collect();
    assert_eq!(metas.len(), 2);
    let meta_body = fs::read_to_string(&metas[0]).unwrap();
    assert!(meta_body.contains("metadata_version: 1"));

    // A scrub.log was written for the secret-bearing record.
    let logs: Vec<_> = walk(&s.archive.join("evidence"))
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("log"))
        .collect();
    assert_eq!(logs.len(), 1, "exactly one record had a secret");
    let log_body = fs::read_to_string(&logs[0]).unwrap();
    assert!(log_body.contains("# evidence scrub log"));
    assert!(!log_body.contains("ghp_"));

    // Idempotency: a second prepare dedups both records via the catalog (the
    // catalog source_digest is the durable backstop, independent of sentinel).
    let report2 = migrate::prepare(&dry_run_args(&s)).expect("prepare 2");
    assert_eq!(report2.skipped, 2, "re-run dedups via catalog");
    assert_eq!(report2.eligible, 0);
}

#[test]
#[cfg(unix)]
fn apply_refuses_dirty_archive() {
    let s = build_scenario();
    // Make the archive dirty under evidence/.
    fs::write(s.archive.join("evidence").join("dirty.txt"), "x").unwrap();
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let err = migrate::apply(&dry_run_args(&s), report).unwrap_err();
    assert_eq!(err.code(), "migrate-archive-repo-dirty");
}

#[test]
#[cfg(unix)]
fn apply_refuses_existing_target() {
    let s = build_scenario();
    // Pre-create one of the target dirs so it "already exists".
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let target = &report.records[0].archive_target.absolute_path;
    fs::create_dir_all(target).unwrap();
    // Commit the empty dir placeholder so the archive isn't dirty.
    fs::write(Path::new(target).join(".keep"), "").unwrap();
    git(&s.archive, &["add", "-A"]);
    git(
        &s.archive,
        &[
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "pre-existing",
        ],
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare again");
    let err = migrate::apply(&dry_run_args(&s), report).unwrap_err();
    assert_eq!(err.code(), "migrate-archive-target-exists");
}

#[test]
fn linked_child_is_copied_scrubbed_and_path_matches_what_was_written() {
    // Added coverage + F3: a non-empty linked_records child is copied, scrubbed,
    // and the rollup `linked_evidence.path` points at the file actually written.
    let s = build_empty_scenario();
    let body = record_json_with_links(
        "deliver-pr",
        "2026-06-14T10:00:00Z",
        r#"[ { "type": "review-evidence", "path": "review.txt" } ]"#,
    );
    write_record_with_child(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &body,
        "review.txt",
        "token glpat-aaaaaaaaaaaaaaaaaaaa more text",
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    assert_eq!(report.eligible, 1);
    let rec = &report.records[0];
    // The rollup references exactly one linked child under the typed subdir.
    assert_eq!(rec.rollup.linked_evidence.len(), 1);
    let linked = &rec.rollup.linked_evidence[0];
    assert_eq!(linked.path, "review-evidence/review.txt");
    // The scrub fired on the child's secret.
    assert!(rec.scrub.total_matches >= 1);

    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare 2");
    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });
    assert_eq!(applied.archived, 1);

    // The staged child exists at the recorded linked_evidence.path and is
    // scrubbed (no raw token, contains the redaction token).
    let target_rel = &applied.targets[0];
    let child_abs = s
        .archive
        .join(target_rel)
        .join("review-evidence")
        .join("review.txt");
    assert!(child_abs.is_file(), "staged child missing at {child_abs:?}");
    let written = fs::read_to_string(&child_abs).unwrap();
    assert!(!written.contains("glpat-"), "raw token leaked: {written}");
    assert!(written.contains("[REDACTED]"), "not scrubbed: {written}");
}

#[test]
fn path_traversal_record_type_cannot_escape_archive_target() {
    // F1: a malicious record_type like `../../../etc` must NOT write outside
    // the archive target; it is sanitized to a safe in-target segment.
    let s = build_empty_scenario();
    let body = record_json_with_links(
        "deliver-pr",
        "2026-06-14T10:00:00Z",
        r#"[ { "type": "../../../etc", "path": "passwd" } ]"#,
    );
    write_record_with_child(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &body,
        "passwd",
        "child body",
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let rec = &report.records[0];
    // The rollup reference is a relative path made of exactly two safe
    // components (`<sanitized-type>/<basename>`) — no path separators became a
    // real directory boundary that escapes, and no component is `..`.
    let linked = &rec.rollup.linked_evidence[0];
    let comps: Vec<&str> = linked.path.split('/').collect();
    assert_eq!(
        comps.len(),
        2,
        "expected one type dir + basename: {linked:?}"
    );
    assert!(
        comps.iter().all(|c| *c != ".." && *c != "."),
        "a path component is a traversal: {}",
        linked.path
    );
    assert_eq!(comps[1], "passwd");

    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare 2");
    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });
    // The child landed strictly inside the archive target dir.
    let target_rel = &applied.targets[0];
    let target_abs = s.archive.join(target_rel);
    let staged: Vec<PathBuf> = walk(&target_abs)
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some("passwd"))
        .collect();
    assert_eq!(staged.len(), 1, "child not staged once under target");
    assert!(
        staged[0].starts_with(&target_abs),
        "child escaped the archive target: {:?}",
        staged[0]
    );
    // Nothing escaped to a sibling of the archive (e.g. <root>/etc/passwd) or
    // to the archive root.
    assert!(!s.root.join("etc").join("passwd").exists());
    assert!(!s.archive.join("etc").join("passwd").exists());
}

#[test]
fn absolute_and_escaping_child_path_is_not_copied() {
    // F2: an absolute `link.path` (e.g. /etc/hosts) and a `../escape` path are
    // recorded as references but NOT copied into the archive.
    let s = build_empty_scenario();
    // Plant a sibling file the run dir should NOT be able to read.
    let secret_dir = s.root.join("secrets");
    fs::create_dir_all(&secret_dir).unwrap();
    fs::write(secret_dir.join("leak.txt"), "TOP SECRET").unwrap();

    let abs = secret_dir.join("leak.txt");
    let abs_str = abs.to_string_lossy().replace('\\', "/");
    let links = format!(
        r#"[ {{ "type": "review-evidence", "path": "{abs_str}" }}, {{ "type": "review-evidence", "path": "../../../secrets/leak.txt" }} ]"#
    );
    let body = record_json_with_links("deliver-pr", "2026-06-14T10:00:00Z", &links);
    write_record(&s.source_out, "graysurf__kit", "20260614-100000", &body);

    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });

    // No staged child anywhere contains the secret bytes.
    let leaked = walk(&s.archive.join("evidence"))
        .into_iter()
        .filter(|p| p.is_file())
        .any(|p| {
            fs::read_to_string(&p)
                .map(|b| b.contains("TOP SECRET"))
                .unwrap_or(false)
        });
    assert!(!leaked, "arbitrary local file was copied into the archive");
    let _ = applied;
}

#[test]
fn linked_path_with_secret_does_not_leak_into_the_on_disk_path() {
    // F3: a secret in a linked PATH must not land in the committed tree path,
    // and the rollup reference must point at the file that was written.
    let s = build_empty_scenario();
    // The basename itself carries a gh token.
    let links = r#"[ { "type": "review-evidence", "path": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.txt" } ]"#;
    let body = record_json_with_links("deliver-pr", "2026-06-14T10:00:00Z", links);
    write_record_with_child(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &body,
        "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.txt",
        "child body",
    );

    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let rec_linked_path = report.records[0].rollup.linked_evidence[0].path.clone();
    // The recorded path must not contain the raw token.
    assert!(
        !rec_linked_path.contains("ghp_"),
        "raw token in rollup path: {rec_linked_path}"
    );
    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });

    // No on-disk archive path contains the raw token.
    let token_in_path = walk(&s.archive.join("evidence"))
        .into_iter()
        .any(|p| p.to_string_lossy().contains("ghp_"));
    assert!(!token_in_path, "raw token leaked into a committed path");

    // The rollup reference points at a file that actually exists on disk.
    let target_rel = &applied.targets[0];
    let referenced = s.archive.join(target_rel).join(&rec_linked_path);
    assert!(
        referenced.is_file(),
        "rollup linked_evidence.path dangles: {referenced:?}"
    );
}

#[test]
fn same_type_same_leaf_children_are_both_preserved() {
    // F5: two children of the same type with the same leaf name must both be
    // preserved at distinct paths, never silently overwritten.
    let s = build_empty_scenario();
    // Two distinct source files whose basename collides after staging:
    // `a/summary.json` and `b/summary.json` both -> review-evidence/summary.json.
    let dir = s
        .source_out
        .join("graysurf__kit")
        .join("20260614-100000-skill-usage");
    fs::create_dir_all(dir.join("a")).unwrap();
    fs::create_dir_all(dir.join("b")).unwrap();
    fs::write(dir.join("a").join("summary.json"), "FIRST").unwrap();
    fs::write(dir.join("b").join("summary.json"), "SECOND").unwrap();
    let links = r#"[ { "type": "review-evidence", "path": "a/summary.json" }, { "type": "review-evidence", "path": "b/summary.json" } ]"#;
    let body = record_json_with_links("deliver-pr", "2026-06-14T10:00:00Z", links);
    fs::write(dir.join("skill-usage.record.json"), body).unwrap();

    configure_push_remote(&s);
    let stub_dir = install_semantic_commit_stub(&s);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    // Two distinct linked_evidence rows with distinct paths.
    let rec = &report.records[0];
    assert_eq!(rec.rollup.linked_evidence.len(), 2);
    let paths: Vec<&str> = rec
        .rollup
        .linked_evidence
        .iter()
        .map(|l| l.path.as_str())
        .collect();
    assert_ne!(paths[0], paths[1], "collision not disambiguated: {paths:?}");

    let applied = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).expect("apply")
    });
    let target_rel = &applied.targets[0];
    // Both bodies survive on disk (no silent single-file overwrite).
    let staged: Vec<String> = walk(&s.archive.join(target_rel).join("review-evidence"))
        .into_iter()
        .filter_map(|p| fs::read_to_string(&p).ok())
        .collect();
    assert!(staged.iter().any(|b| b.contains("FIRST")), "lost FIRST");
    assert!(staged.iter().any(|b| b.contains("SECOND")), "lost SECOND");
}

#[test]
fn filters_by_until_boundary() {
    // Added coverage: `--until` excludes records started after the boundary
    // (inclusive on the boundary day).
    let s = build_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260601-100000",
        &record_json("a", "pass", "early", true, false),
    );
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260620-100000",
        &record_json_with_links("b", "2026-06-20T10:00:00Z", "[]"),
    );

    let mut args = dry_run_args(&s);
    args.until = Some("2026-06-14".to_string());
    let report = migrate::prepare(&args).expect("prepare");
    // Only the early record (2026-06-14 default start) is on/before the until.
    // record_json uses started_at 2026-06-14, record b uses 2026-06-20.
    assert_eq!(report.eligible, 1, "until must exclude the later record");
    assert_eq!(report.records[0].rollup.skill, "a");
}

#[test]
fn promotion_only_keeps_records_with_heuristic_inbox_link() {
    // Added coverage: `--promotion-only` keeps only records that link a
    // heuristic-inbox promotion case.
    let s = build_empty_scenario();
    // A record WITH a heuristic-inbox link.
    let promo = record_json_with_links(
        "heuristic-session-closeout",
        "2026-06-14T10:00:00Z",
        r#"[ { "type": "heuristic-inbox", "path": "https://example/case/42" } ]"#,
    );
    write_record(&s.source_out, "graysurf__kit", "20260614-100000", &promo);
    // A record WITHOUT a promotion link.
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-110000",
        &record_json("deliver-pr", "pass", "no promo", true, false),
    );

    let mut args = dry_run_args(&s);
    args.promotion_only = true;
    let report = migrate::prepare(&args).expect("prepare");
    assert_eq!(report.eligible, 1, "only the promotion record survives");
    let rec = &report.records[0];
    let promotion = rec.rollup.promotion.as_ref().expect("promotion detected");
    assert_eq!(promotion.heuristic_inbox_case, "https://example/case/42");
}

#[test]
#[cfg(unix)]
fn push_failure_records_no_digest_and_re_run_dedups_via_catalog() {
    // Added coverage: when `git push` fails, apply errors out, no digest is
    // recorded in the (now-removed) sentinel, and a re-run still dedups via the
    // catalog written before the push attempt.
    let s = build_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-100000",
        &record_json("deliver-pr", "pass", "deliver", true, false),
    );
    // Install the semantic-commit stub but configure a push remote that
    // REJECTS pushes (a bare repo with receive.denyCurrentBranch is not enough
    // for a bare repo; instead point origin at a non-existent URL so push
    // fails).
    let stub_dir = install_semantic_commit_stub(&s);
    git(
        &s.archive,
        &[
            "remote",
            "add",
            "origin",
            "file:///nonexistent/definitely/not/a/repo.git",
        ],
    );

    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let err = with_path(&stub_dir, || {
        migrate::apply(&dry_run_args(&s), report).unwrap_err()
    });
    assert_eq!(err.code(), "migrate-subprocess-failed");

    // The catalog was regenerated + committed locally before the push, so a
    // re-run dedups the record via the catalog backstop.
    let report2 = migrate::prepare(&dry_run_args(&s)).expect("prepare 2");
    assert_eq!(
        report2.skipped, 1,
        "re-run dedups via catalog after push fail"
    );
    assert_eq!(report2.eligible, 0);
}

#[test]
fn outcome_status_is_scrubbed_in_rollup() {
    // Minor-but-do: outcome.status is scrubbed too.
    let s = build_empty_scenario();
    let body = r#"{
            "schema": "skill-usage.record.v1",
            "producer": { "tool": "skill-usage", "nils_cli_version": "1.4.0" },
            "skill": "deliver-pr",
            "started_at": "2026-06-14T10:00:00Z",
            "ended_at": "2026-06-14T10:30:00Z",
            "cwd": "/Users/tester/Project/kit",
            "trigger": "user_explicit",
            "intent": "intent",
            "inputs": { "user_request_summary": "x", "referenced_files": [], "external_sources": [] },
            "outcome": { "status": "failed token=ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "summary": "done" },
            "artifacts": [],
            "linked_records": [],
            "validation": [],
            "failures": []
        }"#;
    write_record(&s.source_out, "graysurf__kit", "20260614-100000", body);
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let status = &report.records[0].rollup.outcome.status;
    assert!(!status.contains("ghp_"), "status not scrubbed: {status}");
    assert!(status.contains("[REDACTED]"));
}

// --- helpers ---

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn git_count_commits(repo: &Path) -> usize {
    let out = Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git rev-list");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn configure_push_remote(s: &Scenario) {
    let remote = s.root.join("archive-remote.git");
    let out = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote)
        .output()
        .expect("git init --bare");
    assert!(out.status.success());
    git(
        &s.archive,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    git(&s.archive, &["push", "-u", "origin", "main"]);
}

#[cfg(unix)]
fn install_semantic_commit_stub(s: &Scenario) -> PathBuf {
    let bin_dir = s.root.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let stub = bin_dir.join("semantic-commit");
    fs::write(
        &stub,
        r#"#!/bin/sh
repo=
msg=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo) repo="$2"; shift 2 ;;
    -m) msg="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [ -z "$repo" ] || [ -z "$msg" ]; then
  echo "missing repo or message" >&2
  exit 2
fi
git -C "$repo" -c user.name=tester -c user.email=tester@example.com -c commit.gpgsign=false commit -q -m "$msg"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();
    bin_dir
}

/// Run `f` with `dir` prepended to PATH. Serialized via a process-global mutex
/// because PATH is process-wide.
#[cfg(unix)]
fn with_path<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let original = std::env::var("PATH").unwrap_or_default();
    let new = format!("{}:{original}", dir.display());
    // SAFETY: serialized under LOCK; restored before returning.
    unsafe { std::env::set_var("PATH", &new) };
    let result = f();
    unsafe { std::env::set_var("PATH", original) };
    result
}

// ---- review-cleanup regression coverage (PR #848 / #850 bot review) ----

/// A GitHub token shape that `nils-scrub` redacts (ghp_ + 36 chars).
const GHP_TOKEN: &str = "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[test]
fn promotion_case_link_is_scrubbed_before_archiving() {
    // A heuristic-inbox promotion link whose path carries a token must be
    // scrubbed before it lands in the rollup / metadata / catalog.
    let s = build_empty_scenario();
    let links =
        format!(r#"[{{ "type": "heuristic-inbox", "path": "https://example/case/{GHP_TOKEN}" }}]"#);
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-120000",
        &record_json_with_links("deliver-pr", "2026-06-14T12:00:00Z", &links),
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let rec = report
        .records
        .iter()
        .find(|r| r.rollup.promotion.is_some())
        .expect("a promotion record");
    let case = &rec.rollup.promotion.as_ref().unwrap().heuristic_inbox_case;
    assert!(
        !case.contains("ghp_"),
        "promotion link leaked a token: {case}"
    );
    assert!(
        case.contains("[REDACTED]"),
        "promotion link not redacted: {case}"
    );
}

#[test]
fn bare_skill_id_is_scrubbed() {
    // A skill value with no `/` must still be scrubbed, not copied verbatim.
    let s = build_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-130000",
        &record_json_with_links(GHP_TOKEN, "2026-06-14T13:00:00Z", "[]"),
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let rec = &report.records[0];
    assert!(
        !rec.rollup.skill.contains("ghp_"),
        "bare skill id leaked a token: {}",
        rec.rollup.skill
    );
}

#[test]
fn linked_evidence_type_is_scrubbed() {
    // The linked-evidence `type` written into the rollup must be scrubbed.
    let s = build_empty_scenario();
    let links = format!(r#"[{{ "type": "review-evidence-{GHP_TOKEN}", "path": "notes.txt" }}]"#);
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-140000",
        &record_json_with_links("deliver-pr", "2026-06-14T14:00:00Z", &links),
    );
    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    let rec = &report.records[0];
    assert!(
        !rec.rollup
            .linked_evidence
            .iter()
            .any(|l| l.evidence_type.contains("ghp_")),
        "linked-evidence type leaked a token: {:?}",
        rec.rollup
            .linked_evidence
            .iter()
            .map(|l| &l.evidence_type)
            .collect::<Vec<_>>()
    );
}

#[test]
fn unsupported_record_schema_is_skipped_not_normalized_to_v1() {
    // A future/incompatible source schema must be skipped, never silently
    // archived as a `skill-usage.rollup.v1`.
    let s = build_empty_scenario();
    write_record(
        &s.source_out,
        "graysurf__kit",
        "20260614-150000",
        &record_json_with_links("deliver-pr", "2026-06-14T15:00:00Z", "[]"),
    );
    let v2 = record_json_with_links("code-review", "2026-06-14T16:00:00Z", "[]")
        .replace("skill-usage.record.v1", "skill-usage.record.v2");
    write_record(&s.source_out, "graysurf__kit", "20260614-160000", &v2);

    let report = migrate::prepare(&dry_run_args(&s)).expect("prepare");
    assert_eq!(report.eligible, 1, "only the v1 record should be eligible");
    assert!(
        report
            .records
            .iter()
            .all(|r| r.rollup.skill != "code-review"),
        "the unsupported v2 record must not be normalized into a rollup"
    );
    assert_eq!(
        report.blocked.len(),
        1,
        "the v2 record should be recorded as a blocked (skipped) record"
    );
    assert!(
        report.blocked[0].reason.contains("skill-usage.record.v2"),
        "blocked reason should name the unsupported schema: {}",
        report.blocked[0].reason
    );
}
