mod support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;
use serde_json::json;

use support::{Fixture, now_epoch};

const ALLOW_POLICY: &str = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.allow"
products = ["codex"]
events = ["PreToolUse"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "decision.allow.v1", reason_code = "allowed" }
"#;

fn owner_policy() -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.owner"
products = ["codex"]
events = ["PreToolUse"]
matcher = "apply_patch"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = {{ id = "agent-session.owner-liveness.v1", reason_code = "owner", {} = 300 }}
"#,
        concat!("leg", "acy_ttl_seconds")
    )
}

fn owner_policy_with_rules(cardinality: usize) -> String {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..cardinality {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.owner-{index}\"\nproducts = [\"codex\"]\nevents = [\"PreToolUse\"]\nmatcher = \"Write\"\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"agent-session.owner-liveness.v1\", reason_code = \"owner-{index}\", {} = 300 }}\n",
            concat!("leg", "acy_ttl_seconds")
        ));
    }
    policy
}

const TARGET_CARDINALITIES: [usize; 4] = [1, 16, 64, 256];

#[test]
fn ordinary_worktree_target_binding_avoids_git_root_processes() {
    let fixture = Fixture::new(&owner_policy());
    init_repository(&fixture.root);
    let shim = fixture.root.join("shim");
    fs::create_dir(&shim).expect("shim dir");
    let log = fixture.root.join("git-root-lookups.log");
    fs::write(&log, "").expect("lookup log");
    let git = shim.join("git");
    fs::write(
        &git,
        b"#!/bin/sh\ncase \"$*\" in\n  *\"rev-parse --show-toplevel\"*) printf 'root\\n' >> \"$AGENT_HOOK_GIT_ROOT_LOG\" ;;\nesac\nexec \"$AGENT_HOOK_REAL_GIT\" \"$@\"\n",
    )
    .expect("git shim");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git shim mode");
    let real_git = resolve_program("git");
    let path = prepend_path(&shim);
    let log_arg = log.to_string_lossy().into_owned();
    let real_git_arg = real_git.to_string_lossy().into_owned();
    let mut observed = Vec::new();

    for cardinality in TARGET_CARDINALITIES {
        fs::write(&log, "").expect("reset lookup log");
        let payload = apply_patch_payload(&fixture.root, cardinality);
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("PATH", path.as_str()),
                ("AGENT_HOOK_REAL_GIT", real_git_arg.as_str()),
                ("AGENT_HOOK_GIT_ROOT_LOG", log_arg.as_str()),
            ],
        );
        assert!(
            matches!(output.code, 0 | 1),
            "targets={cardinality} stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        observed.push((
            cardinality,
            fs::read_to_string(&log)
                .expect("lookup log")
                .lines()
                .count(),
        ));
    }

    eprintln!("Codex apply_patch root lookups: {observed:?}");
    assert_eq!(
        observed,
        vec![(1, 0), (16, 0), (64, 0), (256, 0)],
        "ordinary worktree target binding must not spawn git rev-parse"
    );
}

#[test]
fn linked_worktree_target_binding_avoids_git_root_processes() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let primary = fixture.root.join("primary");
    let linked = fixture.root.join("linked");
    fs::create_dir(&primary).expect("primary checkout");
    nils_test_support::git::init_repo_at_with(
        &primary,
        nils_test_support::git::InitRepoOptions::new().with_initial_commit(),
    );
    nils_test_support::git::worktree_add_branch(&primary, &linked, "linked-test");

    let shim = fixture.root.join("shim");
    fs::create_dir(&shim).expect("shim dir");
    let log = fixture.root.join("git-root-lookups.log");
    fs::write(&log, "").expect("lookup log");
    let git = shim.join("git");
    fs::write(
        &git,
        b"#!/bin/sh\ncase \"$*\" in\n  *\"rev-parse --show-toplevel\"*) printf 'root\\n' >> \"$AGENT_HOOK_GIT_ROOT_LOG\" ;;\nesac\nexec \"$AGENT_HOOK_REAL_GIT\" \"$@\"\n",
    )
    .expect("git shim");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git shim mode");
    let real_git = resolve_program("git");
    let path = prepend_path(&shim);
    let log_arg = log.to_string_lossy().into_owned();
    let real_git_arg = real_git.to_string_lossy().into_owned();
    let payload = apply_patch_payload(&linked, 1);
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("PATH", path.as_str()),
            ("AGENT_HOOK_REAL_GIT", real_git_arg.as_str()),
            ("AGENT_HOOK_GIT_ROOT_LOG", log_arg.as_str()),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        fs::read_to_string(&log)
            .expect("lookup log")
            .lines()
            .count(),
        0,
        "linked worktree target binding must not spawn git rev-parse"
    );
}

#[test]
fn separate_git_directory_retains_external_git_root_fallback() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let checkout = fixture.root.join("separate-checkout");
    let git_dir = fixture.root.join("separate-git-dir");
    let status = Command::new(resolve_program("git"))
        .args(["init", "-q", "--separate-git-dir"])
        .arg(&git_dir)
        .arg(&checkout)
        .status()
        .expect("git init with separate git dir");
    assert!(status.success());

    let shim = fixture.root.join("shim");
    fs::create_dir(&shim).expect("shim dir");
    let log = fixture.root.join("git-root-lookups.log");
    fs::write(&log, "").expect("lookup log");
    let git = shim.join("git");
    fs::write(
        &git,
        b"#!/bin/sh\ncase \"$*\" in\n  *\"rev-parse --show-toplevel\"*) printf 'root\\n' >> \"$AGENT_HOOK_GIT_ROOT_LOG\" ;;\nesac\nexec \"$AGENT_HOOK_REAL_GIT\" \"$@\"\n",
    )
    .expect("git shim");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git shim mode");
    let real_git = resolve_program("git");
    let path = prepend_path(&shim);
    let log_arg = log.to_string_lossy().into_owned();
    let real_git_arg = real_git.to_string_lossy().into_owned();
    let payload = apply_patch_payload(&checkout, 1);
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("PATH", path.as_str()),
            ("AGENT_HOOK_REAL_GIT", real_git_arg.as_str()),
            ("AGENT_HOOK_GIT_ROOT_LOG", log_arg.as_str()),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        fs::read_to_string(&log)
            .expect("lookup log")
            .lines()
            .count(),
        1,
        "ambiguous but valid repository layouts must retain the Git fallback"
    );
}

#[test]
fn invalid_git_directory_retains_external_git_root_fallback() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let checkout = fixture.root.join("invalid-checkout");
    fs::create_dir(&checkout).expect("checkout");
    fs::create_dir(checkout.join(".git")).expect("invalid .git directory");

    let shim = fixture.root.join("shim");
    fs::create_dir(&shim).expect("shim dir");
    let log = fixture.root.join("git-root-lookups.log");
    fs::write(&log, "").expect("lookup log");
    let git = shim.join("git");
    fs::write(
        &git,
        b"#!/bin/sh\ncase \"$*\" in\n  *\"rev-parse --show-toplevel\"*) printf 'root\\n' >> \"$AGENT_HOOK_GIT_ROOT_LOG\" ;;\nesac\nexec \"$AGENT_HOOK_REAL_GIT\" \"$@\"\n",
    )
    .expect("git shim");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git shim mode");
    let real_git = resolve_program("git");
    let path = prepend_path(&shim);
    let log_arg = log.to_string_lossy().into_owned();
    let real_git_arg = real_git.to_string_lossy().into_owned();
    let payload = apply_patch_payload(&checkout, 1);
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
        &[
            ("PATH", path.as_str()),
            ("AGENT_HOOK_REAL_GIT", real_git_arg.as_str()),
            ("AGENT_HOOK_GIT_ROOT_LOG", log_arg.as_str()),
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    assert_eq!(
        fs::read_to_string(&log)
            .expect("lookup log")
            .lines()
            .count(),
        2,
        "an invalid ordinary .git directory must retain Git's uncached not-a-repository decision"
    );
}

#[test]
fn coordination_rule_cardinality_bounds_expensive_liveness_io() {
    for (cardinality, expected_exit, expected_status_probes) in
        [(1, 1, 1), (16, 1, 1), (64, 1, 1), (65, 65, 0), (512, 65, 0)]
    {
        let fixture = Fixture::new(&owner_policy_with_rules(cardinality));
        init_repository(&fixture.root);
        let shim = fixture.root.join("status-shim");
        fs::create_dir(&shim).expect("shim dir");
        let log = fixture.root.join("git-status.log");
        fs::write(&log, "").expect("status log");
        let git = shim.join("git");
        fs::write(
            &git,
            b"#!/bin/sh\ncase \" $* \" in\n  *\" status --porcelain=v1 \"*) printf 'status\\n' >> \"$AGENT_HOOK_GIT_STATUS_LOG\" ;;\nesac\nexec \"$AGENT_HOOK_REAL_GIT\" \"$@\"\n",
        )
        .expect("git shim");
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("git shim mode");
        let real_git = resolve_program("git");
        let path = prepend_path(&shim);
        let log_arg = log.to_string_lossy().into_owned();
        let real_git_arg = real_git.to_string_lossy().into_owned();
        let payload = json!({
            "hook_event_name":"PreToolUse",
            "tool_name":"Write",
            "cwd":fixture.root,
            "tool_input":{"path":fixture.root.join("target.txt")}
        })
        .to_string();
        let output = fixture.run_with_env(
            &["dispatch", "--product", "codex", "--format", "json"],
            Some(&payload),
            &[
                ("PATH", path.as_str()),
                ("AGENT_HOOK_REAL_GIT", real_git_arg.as_str()),
                ("AGENT_HOOK_GIT_STATUS_LOG", log_arg.as_str()),
                ("AGENT_SESSION_ID", "current"),
                ("AGENT_SESSION_RUNTIME_ID", "inc-current"),
            ],
        );
        assert_eq!(
            output.code,
            expected_exit,
            "rules={cardinality} stdout={} stderr={}",
            output.stdout_text(),
            output.stderr_text()
        );
        if cardinality > 64 {
            assert_eq!(
                output.stdout_json()["error"]["code"],
                "decision-reason-limit"
            );
        }
        let probes = fs::read_to_string(&log)
            .expect("status log")
            .lines()
            .count();
        assert_eq!(
            probes, expected_status_probes,
            "rules={cardinality} must not amplify git status per selected rule"
        );
    }
}

#[test]
fn codex_apply_patch_target_binding_meets_dispatch_latency_budget() {
    let fixture = Fixture::new(ALLOW_POLICY);
    init_repository(&fixture.root);
    let payloads = TARGET_CARDINALITIES
        .into_iter()
        .map(|cardinality| (cardinality, apply_patch_payload(&fixture.root, cardinality)))
        .collect::<Vec<_>>();
    let warmup = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payloads[0].1),
    );
    assert_eq!(warmup.code, 0, "stderr={}", warmup.stderr_text());

    let mut p95 = Vec::new();
    for (cardinality, payload) in payloads {
        let mut samples = Vec::with_capacity(20);
        for _ in 0..20 {
            let started = Instant::now();
            let output = fixture.run(
                &["dispatch", "--product", "codex", "--format", "json"],
                Some(&payload),
            );
            samples.push(started.elapsed());
            assert_eq!(
                output.code,
                0,
                "targets={cardinality} stderr={}",
                output.stderr_text()
            );
        }
        samples.sort_unstable();
        p95.push((cardinality, samples[18]));
    }

    let limits = [
        (1, Duration::from_millis(25)),
        (16, Duration::from_millis(25)),
        (64, Duration::from_millis(50)),
        (256, Duration::from_millis(100)),
    ];
    eprintln!("Codex apply_patch dispatch p95: {p95:?}");
    let violations = p95
        .iter()
        .zip(limits)
        .filter_map(|((cardinality, observed), (expected_cardinality, limit))| {
            assert_eq!(*cardinality, expected_cardinality);
            (*observed > limit).then_some((*cardinality, *observed, limit))
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "Codex apply_patch p95 latency exceeded budgets: observed={p95:?} violations={violations:?}"
    );
}

#[test]
fn codex_foreign_marker_validation_rejects_near_limit_reverse_close_linearly() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let codex = fixture.home.join(".codex");
    fs::create_dir_all(&codex).expect("codex dir");
    let config = codex.join("config.toml");
    let mut malformed = String::from("model = \"gpt-test\"\n");
    for index in 0..17_000 {
        malformed.push_str(&format!("# >>> foreign-{index}:hooks >>>\n"));
    }
    for index in 0..17_000 {
        malformed.push_str(&format!("# <<< foreign-{index}:hooks <<<\n"));
    }
    assert!(
        (900 * 1024..1024 * 1024).contains(&malformed.len()),
        "fixture bytes={}",
        malformed.len()
    );
    fs::write(&config, &malformed).expect("malformed Codex config");
    Fixture::set_private(&config);

    let started = Instant::now();
    let output = fixture.run(
        &[
            "setup",
            "--product",
            "codex",
            "--dry-run",
            "--format",
            "json",
        ],
        None,
    );
    let elapsed = started.elapsed();
    eprintln!(
        "Codex reverse-close marker rejection: bytes={} elapsed={elapsed:?}",
        malformed.len()
    );

    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "provider-config-invalid"
    );
    assert!(
        elapsed <= Duration::from_millis(500),
        "near-limit reverse-close marker validation took {elapsed:?} for {} bytes",
        malformed.len()
    );
}

fn init_repository(root: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(root)
        .status()
        .expect("git init");
    assert!(status.success());
}

fn apply_patch_payload(root: &Path, cardinality: usize) -> String {
    let mut patch = String::from("*** Begin Patch\n");
    for index in 0..cardinality {
        patch.push_str(&format!("*** Add File: target-{index}.txt\n+value\n"));
    }
    patch.push_str("*** End Patch");
    json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"apply_patch",
        "cwd":root,
        "tool_input":{"command":patch}
    })
    .to_string()
}

fn resolve_program(program: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| fs::canonicalize(candidate).ok())
        .unwrap_or_else(|| panic!("{program} is unavailable"))
}

fn prepend_path(directory: &Path) -> String {
    let mut paths =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    paths.insert(0, directory.to_path_buf());
    std::env::join_paths(paths)
        .expect("join PATH")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn coordination_irrelevant_dispatch_does_not_read_the_registry() {
    let fixture = Fixture::new(ALLOW_POLICY);
    let coordination = fixture.session_state.join("coordination");
    fs::create_dir_all(&coordination).expect("coordination dir");
    let registry = coordination.join("registry.json");
    fs::write(
        &registry,
        br#"{"schema_version":"agent-session.coordination-registry.v1","fingerprint_epoch":1,"fingerprint_key":"0123456789abcdef0123456789abcdef","brokers":{},"claims":[]}"#,
    )
    .expect("registry");
    Fixture::set_private(&registry);
    let old = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: registry is a valid NUL-terminated path and the timespec array is live.
    let path = std::ffi::CString::new(registry.as_os_str().as_encoded_bytes()).expect("path");
    assert_eq!(
        unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), [old, old].as_ptr(), 0) },
        0
    );
    let before = fs::metadata(&registry).expect("metadata").atime();
    let payload = json!({
        "hook_event_name":"PreToolUse",
        "tool_name":"Read",
        "cwd":fixture.root
    })
    .to_string();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(&payload),
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let after = fs::metadata(&registry).expect("metadata").atime();
    assert_eq!(
        after, before,
        "coordination-irrelevant dispatch read registry.json"
    );
}

#[test]
fn executable_capabilities_share_one_cardinality_and_deadline_budget() {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..70 {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.child-{index}\"\nproducts = [\"codex\"]\nevents = [\"SessionStart\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"runtime-kit.handler.v1\", handler_id = \"session-start-healthcheck\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(
        &handler,
        b"#!/bin/sh\nprintf 'start\\n' >> \"$AGENT_HOOK_CHILD_LOG\"\nprintf '{}\\n'\n",
    )
    .expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    let log = fixture.root.join("child.log");
    let log_arg = log.to_string_lossy().into_owned();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
        &[("AGENT_HOOK_CHILD_LOG", log_arg.as_str())],
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dispatch-child-budget-exceeded"
    );
    let starts = fs::read_to_string(&log).unwrap_or_default().lines().count();
    assert!(
        starts <= 17,
        "started {starts} children after the dispatch budget"
    );
}

#[test]
fn child_deadline_is_dispatch_wide_and_below_provider_timeout() {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..3 {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.slow-{index}\"\nproducts = [\"codex\"]\nevents = [\"SessionStart\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"runtime-kit.handler.v1\", handler_id = \"session-start-healthcheck\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(&handler, b"#!/bin/sh\nsleep 2\nprintf '{}\\n'\n").expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    let started = Instant::now();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "dispatch-deadline-exceeded"
    );
    assert!(started.elapsed() < Duration::from_millis(5_800));
}

#[test]
fn full_handler_set_tolerates_one_slow_probe_with_process_overhead() {
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..17 {
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.realistic-{index}\"\nproducts = [\"codex\"]\nevents = [\"SessionStart\"]\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"runtime-kit.handler.v1\", handler_id = \"session-start-healthcheck\" }}\n"
        ));
    }
    let fixture = Fixture::new(&policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(
        &handler,
        b"#!/bin/sh\nif [ ! -e \"$AGENT_HOOK_SLOW_MARKER\" ]; then\n  : > \"$AGENT_HOOK_SLOW_MARKER\"\n  sleep 1\nfi\nsleep 0.08\nprintf 'start\\n' >> \"$AGENT_HOOK_CHILD_LOG\"\nprintf '{}\\n'\n",
    )
    .expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");
    let log = fixture.root.join("child.log");
    let marker = fixture.root.join("slow.marker");
    let log_arg = log.to_string_lossy().into_owned();
    let marker_arg = marker.to_string_lossy().into_owned();
    let output = fixture.run_with_env(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
        &[
            ("AGENT_HOOK_CHILD_LOG", log_arg.as_str()),
            ("AGENT_HOOK_SLOW_MARKER", marker_arg.as_str()),
        ],
    );
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let starts = fs::read_to_string(&log).unwrap_or_default().lines().count();
    assert_eq!(starts, 17, "every selected handler must complete");
}

#[test]
fn handler_descendants_cannot_retain_pipes_beyond_the_dispatch_deadline() {
    let policy = r#"schema_version = "agent-hook.policy.v1"
bundle_id = "runtime-kit"
version = "2026.07.20.1"

[[rules]]
id = "runtime.descendant"
products = ["codex"]
events = ["SessionStart"]
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "locked"
capability = { id = "runtime-kit.handler.v1", handler_id = "session-start-healthcheck" }
"#;
    let fixture = Fixture::new(policy);
    let hooks = fixture.home.join(".codex/hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    let handler = hooks.join("session-start-healthcheck.sh");
    fs::write(&handler, b"#!/bin/sh\nsleep 30 &\nprintf '{}\\n'\n").expect("handler");
    fs::set_permissions(&handler, fs::Permissions::from_mode(0o755)).expect("handler mode");

    let started = Instant::now();
    let output = fixture.run(
        &["dispatch", "--product", "codex", "--format", "json"],
        Some(r#"{"hook_event_name":"SessionStart","source":"startup"}"#),
    );

    assert!(
        matches!(output.code, 0 | 1),
        "stdout={} stderr={}",
        output.stdout_text(),
        output.stderr_text()
    );
    assert!(started.elapsed() < Duration::from_millis(5_800));
}

#[test]
fn setup_never_emits_a_provider_candidate_larger_than_its_own_read_limit() {
    let events = [
        "SessionStart",
        "PermissionRequest",
        "PreToolUse",
        "PostToolUse",
        "PreCompact",
        "PostCompact",
        "SubagentStart",
        "SubagentStop",
    ];
    let mut policy = String::from(
        "schema_version = \"agent-hook.policy.v1\"\nbundle_id = \"runtime-kit\"\nversion = \"2026.07.20.1\"\n",
    );
    for index in 0..256 {
        let matcher = (0..8)
            .map(|atom| format!("m{index}-{atom}-{}", "x".repeat(116)))
            .collect::<Vec<_>>()
            .join("|");
        policy.push_str(&format!(
            "\n[[rules]]\nid = \"runtime.large-{index}\"\nproducts = [\"codex\"]\nevents = [{}]\nmatcher = \"{matcher}\"\npriority = {index}\nmode = \"enforce\"\nfailure_posture = \"closed\"\noverride_class = \"locked\"\ncapability = {{ id = \"decision.allow.v1\", reason_code = \"large-{index}\" }}\n",
            events.iter().map(|event| format!("\"{event}\"")).collect::<Vec<_>>().join(", ")
        ));
    }
    assert!(policy.len() < 1024 * 1024);
    let fixture = Fixture::new(&policy);
    let output = fixture.run(
        &["setup", "--product", "codex", "--apply", "--format", "json"],
        None,
    );
    assert_eq!(output.code, 65, "stderr={}", output.stderr_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "provider-config-candidate-too-large"
    );
    assert!(!fixture.home.join(".codex/config.toml").exists());
}

#[test]
fn performance_fixture_clock_is_sane() {
    assert!(now_epoch() > 0);
}
