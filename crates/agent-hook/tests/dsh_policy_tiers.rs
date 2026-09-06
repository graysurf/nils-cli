mod support;

use std::fs;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use support::Fixture;

const RULE_ID: &str = "dsh.tiered";

fn policy_for(group: &str, override_class: &str) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "dsh-runtime-kit-tiers"
version = "2026.09.06.1"

[[rules]]
id = "{RULE_ID}"
products = ["dsh"]
events = ["PreToolUse"]
matcher = "bash"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "{override_class}"
capability = {{ id = "dsh.policy.v1", group = "{group}" }}
"#,
    )
}

fn policy_for_groups(groups: &[(&str, &str)]) -> String {
    let mut policy = String::from(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "dsh-runtime-kit-tiers"
version = "2026.09.06.1"
"#,
    );
    for (index, (group, override_class)) in groups.iter().enumerate() {
        policy.push_str(&format!(
            r#"
[[rules]]
id = "dsh.tiered-{index}"
products = ["dsh"]
events = ["PreToolUse"]
matcher = "bash"
priority = 10
mode = "enforce"
failure_posture = "closed"
override_class = "{override_class}"
capability = {{ id = "dsh.policy.v1", group = "{group}" }}
"#,
        ));
    }
    policy
}

fn add_override(fixture: &Fixture, rule_id: &str, mode: &str) {
    let mut config = fs::read_to_string(&fixture.config).expect("config");
    config.push_str(&format!("\n[overrides.\"{rule_id}\"]\nmode = \"{mode}\"\n"));
    fs::write(&fixture.config, config).expect("config with override");
    Fixture::set_private(&fixture.config);
}

fn request(fixture: &Fixture, session_id: &str, command: &str) -> String {
    json!({
        "schema_version": "agent-hook.dsh-ingress.v2",
        "event": "tools/pre-execute",
        "call_id": "dsh-tier-call",
        "cwd": fixture.root,
        "subject": {
            "session_id": session_id,
            "turn": 1,
            "step": 2,
            "agent_docs_state_home": fixture.state_home.join("dsh-runtime-kit")
        },
        "tool": {
            "name": "bash",
            "arguments": { "command": command }
        }
    })
    .to_string()
}

fn dispatch(fixture: &Fixture, session_id: &str, command: &str) -> (i32, Value) {
    let output = fixture.run(
        &["dispatch", "--product", "dsh", "--format", "json"],
        Some(&request(fixture, session_id, command)),
    );
    (output.code, output.stdout_json())
}

const TIER_B_BLOCKING: [(&str, &str); 6] = [
    (
        "block-direct-git-commit",
        "git commit --allow-empty -m bypass",
    ),
    ("block-direct-git-worktree", "git worktree remove ../other"),
    ("block-direct-pr-create", "gh pr create --draft"),
    (
        "semantic-commit-body-gate",
        "semantic-commit commit --type feat --subject bypass",
    ),
    ("block-unsafe-default-delivery", "git push origin main"),
    (
        "portable-paths-scan",
        "printf '%s\\n' /Users/someone/project >> notes.md",
    ),
];

#[test]
fn tier_b_enforce_blocks_and_names_the_governed_replacement() {
    for (group, command) in TIER_B_BLOCKING {
        let fixture = Fixture::new(&policy_for(group, "downgrade-only"));
        let (code, envelope) = dispatch(&fixture, "dsh-session-1", command);
        assert_eq!(code, 1, "group={group} envelope={envelope}");
        assert_eq!(envelope["data"]["action"], "block", "group={group}");
        assert_eq!(envelope["data"]["reasons"][0]["code"], group);
        assert_eq!(envelope["data"]["enforcement"], "block", "group={group}");
        assert!(
            envelope["data"].get("downgraded_by").is_none(),
            "group={group}: an enforced block carries no downgrade source"
        );
        let context = envelope["data"]["context"]
            .as_str()
            .unwrap_or_else(|| panic!("group={group}: a Tier B denial names its remediation"));
        assert!(
            context.contains("semantic-commit")
                || context.contains("git-cli")
                || context.contains("forge-cli")
                || context.contains("$HOME"),
            "group={group}: remediation must name the governed replacement: {context}"
        );
    }
}

#[test]
fn advise_projects_every_tier_b_block_to_context_with_the_downgrade_source() {
    for (group, command) in TIER_B_BLOCKING {
        let fixture = Fixture::new(&policy_for(group, "downgrade-only"));
        add_override(&fixture, RULE_ID, "advise");
        let (code, envelope) = dispatch(&fixture, "dsh-session-1", command);
        assert_eq!(code, 0, "group={group} envelope={envelope}");
        assert_eq!(envelope["data"]["action"], "context", "group={group}");
        assert_eq!(envelope["data"]["reasons"][0]["code"], group);
        assert_eq!(envelope["data"]["reasons"][0]["disposition"], "context");
        assert_eq!(envelope["data"]["enforcement"], "advise", "group={group}");
        let downgraded_by = envelope["data"]["downgraded_by"]
            .as_str()
            .unwrap_or_else(|| panic!("group={group}: downgraded_by is set"));
        let config = fixture.config.to_string_lossy();
        assert_eq!(downgraded_by, format!("{config} [overrides.{RULE_ID}]"));
        let context = envelope["data"]["context"].as_str().expect("context");
        assert!(
            context.ends_with(&format!(
                "downgraded to advise by {config} [overrides.{RULE_ID}]"
            )),
            "group={group}: context must end with the downgrade line: {context}"
        );
    }
}

#[test]
fn advise_keeps_allow_and_context_outcomes_unchanged() {
    let fixture = Fixture::new(&policy_for("block-direct-git-commit", "downgrade-only"));
    add_override(&fixture, RULE_ID, "advise");
    let (code, envelope) = dispatch(&fixture, "dsh-session-1", "git status --short");
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["action"], "allow");
    assert!(envelope["data"].get("enforcement").is_none());
    assert!(envelope["data"].get("downgraded_by").is_none());
}

#[test]
fn tier_a_rules_cannot_be_downgraded_by_declaration_or_override() {
    for group in [
        "owner-unclaimed",
        "semantic-conflict",
        "operation-lifecycle",
        "agent-scope-lock-guard",
        "checkout-lease-guard",
        "mcp-secret-scan",
    ] {
        // finish-line-record has no PreToolUse binding; tests/policy_parity.rs
        // freezes its tier.
        let fixture = Fixture::new(&policy_for(group, "downgrade-only"));
        let output = fixture.run(&["validate", "--format", "json"], None);
        assert_eq!(
            output.code,
            65,
            "group={group} stdout={}",
            output.stdout_text()
        );
        let envelope = output.stdout_json();
        let code = envelope["error"]["code"].as_str().unwrap_or_default();
        assert!(
            matches!(
                code,
                "tier-a-rule-not-locked" | "coordination-rule-not-locked"
            ),
            "group={group}: a Tier A group must fail closed as locked, got {code}"
        );
    }
    let fixture = Fixture::new(&policy_for("checkout-lease-guard", "locked"));
    add_override(&fixture, RULE_ID, "advise");
    let output = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65);
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "locked-rule-override"
    );
}

#[test]
fn tier_b_rules_reject_a_free_declaration_and_tier_c_rules_must_stay_locked() {
    let fixture = Fixture::new(&policy_for("block-direct-git-commit", "free"));
    let output = fixture.run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "tier-b-rule-not-downgradable"
    );
    let locked = Fixture::new(&policy_for("block-direct-git-commit", "locked"));
    let output = locked.run(&["validate", "--format", "json"], None);
    assert_eq!(
        output.code,
        0,
        "a locked Tier B declaration stays valid: {}",
        output.stdout_text()
    );
    for group in ["forge-label-reminder", "block-direct-python"] {
        let fixture = Fixture::new(&policy_for(group, "downgrade-only"));
        let output = fixture.run(&["validate", "--format", "json"], None);
        assert_eq!(output.code, 65, "group={group}");
        assert_eq!(
            output.stdout_json()["error"]["code"],
            "tier-c-rule-not-locked",
            "group={group}"
        );
    }
}

#[test]
fn inventory_reports_the_tier_and_default_enforcement_of_every_dsh_rule() {
    let fixture = Fixture::new(&policy_for_groups(&[
        ("checkout-lease-guard", "locked"),
        ("block-direct-git-commit", "downgrade-only"),
        ("forge-label-reminder", "locked"),
    ]));
    add_override(&fixture, "dsh.tiered-1", "advise");
    let output = fixture.run(&["inventory", "--format", "json"], None);
    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let rules = output.stdout_json()["data"]["rules"].clone();
    assert_eq!(rules[0]["tier"], "integrity");
    assert_eq!(rules[0]["enforcement_default"], "block");
    assert_eq!(rules[1]["tier"], "governed-seam");
    assert_eq!(rules[1]["enforcement_default"], "block");
    assert_eq!(rules[1]["effective_modes"]["dsh"], "advise");
    assert_eq!(rules[2]["tier"], "reminder");
    assert_eq!(rules[2]["enforcement_default"], "context");
}

#[test]
fn direct_python_under_a_managed_project_is_advisory() {
    let fixture = Fixture::new(&policy_for("block-direct-python", "locked"));
    fs::write(fixture.root.join("uv.lock"), "version = 1\n").expect("uv marker");
    let (code, envelope) = dispatch(&fixture, "dsh-session-1", "python -c 'print(1)'");
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["action"], "context");
    assert_eq!(
        envelope["data"]["reasons"][0]["code"],
        "block-direct-python"
    );
    assert_eq!(envelope["data"]["enforcement"], "context");
    let context = envelope["data"]["context"].as_str().expect("advisory text");
    assert!(
        context.contains("uv"),
        "names the detected manager: {context}"
    );
    assert!(
        context.contains("uv run") && context.contains(".venv/bin/python"),
        "names the expected interpreter path: {context}"
    );

    let venv = Fixture::new(&policy_for("block-direct-python", "locked"));
    fs::create_dir_all(venv.root.join(".venv")).expect("venv dir");
    fs::write(venv.root.join(".venv/pyvenv.cfg"), "home = /usr/bin\n").expect("venv marker");
    let (_, envelope) = dispatch(&venv, "dsh-session-1", "python3 -c 'print(1)'");
    assert_eq!(envelope["data"]["action"], "context");
    assert!(
        envelope["data"]["context"]
            .as_str()
            .expect("advisory text")
            .contains("venv"),
        "names the detected manager"
    );

    // Outside a managed project a bare interpreter is still an unclassifiable
    // command consumer, so the Tier C reminder is the shell retry guidance
    // rather than a manager hint, and it never blocks.
    let unmanaged = Fixture::new(&policy_for("block-direct-python", "locked"));
    let (code, envelope) = dispatch(&unmanaged, "dsh-session-1", "python -c 'print(1)'");
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["action"], "context");
    let context = envelope["data"]["context"]
        .as_str()
        .expect("shell guidance");
    assert!(!context.contains("managed by"), "{context}");
    assert!(context.contains("could not be classified"), "{context}");
    assert!(!context.contains("was blocked"), "{context}");
    let (_, envelope) = dispatch(&unmanaged, "dsh-session-1", "uv run python -c 'print(1)'");
    assert_eq!(envelope["data"]["action"], "allow");
}

#[test]
fn unclassifiable_shell_blocks_only_tier_b_groups_whose_subject_appears() {
    let fixture = Fixture::new(&policy_for_groups(&[
        ("block-direct-git-commit", "downgrade-only"),
        ("portable-paths-scan", "downgrade-only"),
        ("block-direct-python", "locked"),
        ("checkout-lease-guard", "locked"),
    ]));
    fs::write(fixture.root.join("uv.lock"), "version = 1\n").expect("uv marker");

    let (code, envelope) = dispatch(
        &fixture,
        "dsh-session-1",
        "export EDITOR=vi; sed -i 's/a/b/' notes.md",
    );
    let reasons = envelope["data"]["reasons"].as_array().expect("reasons");
    let disposition = |group: &str| {
        reasons
            .iter()
            .find(|reason| reason["code"] == group)
            .map(|reason| {
                reason["disposition"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_else(|| panic!("group {group} missing in {envelope}"))
    };
    assert_eq!(
        code, 1,
        "the Tier A lease guard still fails closed: {envelope}"
    );
    assert_eq!(disposition("checkout-lease-guard"), "block");
    assert_eq!(disposition("block-direct-git-commit"), "context");
    assert_eq!(disposition("portable-paths-scan"), "context");
    assert_eq!(disposition("block-direct-python"), "context");

    let narrowed = Fixture::new(&policy_for_groups(&[
        ("block-direct-git-commit", "downgrade-only"),
        ("portable-paths-scan", "downgrade-only"),
    ]));
    let (code, envelope) = dispatch(
        &narrowed,
        "dsh-session-1",
        "export EDITOR=vi; sed -i 's/a/b/' notes.md",
    );
    assert_eq!(
        code, 0,
        "no Tier B subject appears, so nothing blocks: {envelope}"
    );
    assert_eq!(envelope["data"]["action"], "context");
    assert!(
        envelope["data"]["context"]
            .as_str()
            .expect("retry guidance")
            .contains("separate Bash tool calls")
    );

    let (code, envelope) = dispatch(&narrowed, "dsh-session-2", "export EDITOR=vi; git status");
    assert_eq!(
        code, 1,
        "the git subject appears, so the seam blocks: {envelope}"
    );
    let reasons = envelope["data"]["reasons"].as_array().expect("reasons");
    assert_eq!(reasons[0]["code"], "block-direct-git-commit");
    assert_eq!(reasons[0]["disposition"], "block");
    assert_eq!(reasons[1]["code"], "portable-paths-scan");
    assert_eq!(reasons[1]["disposition"], "context");
    assert!(
        envelope["data"]["context"]
            .as_str()
            .expect("denial guidance")
            .contains("was blocked before command dispatch"),
        "a denial keeps the blocked wording: {envelope}"
    );
}

#[test]
fn quoted_or_escaped_subjects_still_keep_a_tier_b_seam_failing_closed() {
    // shell_words::split unquotes these to `git`/`gh`; the subject check must
    // see the same token rather than the raw spelling.
    for (group, command) in [
        (
            "block-direct-git-commit",
            "export EDITOR=vi; \\git commit -m x",
        ),
        (
            "block-direct-git-commit",
            "export EDITOR=vi; g''it commit -m x",
        ),
        (
            "block-direct-git-commit",
            "export EDITOR=vi; \"g\"it commit -m x",
        ),
        (
            "block-direct-git-commit",
            "export EDITOR=vi; gi\\t commit -m x",
        ),
        (
            "block-direct-git-worktree",
            "cd /tmp; /usr/bin/g'i't worktree remove ../x",
        ),
        ("block-direct-pr-create", "export EDITOR=vi; \\gh pr create"),
        (
            "semantic-commit-body-gate",
            "export EDITOR=vi; semantic-\"commit\" commit -m x",
        ),
    ] {
        let fixture = Fixture::new(&policy_for(group, "downgrade-only"));
        let (code, envelope) = dispatch(&fixture, "dsh-session-1", command);
        assert_eq!(
            code, 1,
            "group={group} command={command} envelope={envelope}"
        );
        assert_eq!(envelope["data"]["action"], "block", "command={command}");
        assert_eq!(envelope["data"]["reasons"][0]["code"], group);
    }
}

#[test]
fn content_seams_keep_failing_closed_when_an_unclassifiable_command_names_their_subject() {
    let memory = Fixture::new(&policy_for("block-project-memory-write", "downgrade-only"));
    let (code, envelope) = dispatch(
        &memory,
        "dsh-session-1",
        "export EDITOR=vi; perl -pi -e 's/a/b/' .config/agent-memory/candidates/dsh/notes.md",
    );
    assert_eq!(code, 1, "a memory note is named: {envelope}");
    assert_eq!(envelope["data"]["action"], "block");
    let (code, envelope) = dispatch(
        &memory,
        "dsh-session-2",
        "export EDITOR=vi; perl -pi -e 's/a/b/' notes.md",
    );
    assert_eq!(code, 0, "no memory path is named: {envelope}");
    assert_eq!(envelope["data"]["action"], "context");

    let portable = Fixture::new(&policy_for("portable-paths-scan", "downgrade-only"));
    let (code, envelope) = dispatch(
        &portable,
        "dsh-session-1",
        "export EDITOR=vi; sed -i 's#x#/Users/someone/project#' README.md",
    );
    assert_eq!(code, 1, "a machine-local path is named: {envelope}");
    assert_eq!(envelope["data"]["action"], "block");
    let (code, envelope) = dispatch(
        &portable,
        "dsh-session-2",
        "export EDITOR=vi; sed -i 's#x#$HOME/project#' README.md",
    );
    assert_eq!(code, 0, "a portable path is fine: {envelope}");
    assert_eq!(envelope["data"]["action"], "context");
}

#[test]
fn a_natural_context_under_an_advise_rule_is_not_reported_as_a_downgrade() {
    let fixture = Fixture::new(&policy_for("block-direct-git-commit", "downgrade-only"));
    add_override(&fixture, RULE_ID, "advise");
    let (code, envelope) = dispatch(
        &fixture,
        "dsh-session-1",
        "export EDITOR=vi; sed -i 's/a/b/' notes.md",
    );
    assert_eq!(code, 0, "envelope={envelope}");
    assert_eq!(envelope["data"]["action"], "context");
    assert_eq!(envelope["data"]["enforcement"], "context");
    assert!(envelope["data"].get("downgraded_by").is_none());
    assert!(
        !envelope["data"]["context"]
            .as_str()
            .expect("context")
            .contains("downgraded to advise"),
        "{envelope}"
    );
}

fn codex_block_policy(mode: &str) -> String {
    format!(
        r#"schema_version = "agent-hook.policy.v1"
bundle_id = "codex-advise"
version = "2026.09.06.1"

[[rules]]
id = "{RULE_ID}"
products = ["codex"]
events = ["PermissionRequest"]
priority = 10
mode = "{mode}"
failure_posture = "closed"
override_class = "downgrade-only"
capability = {{ id = "decision.block.v1", reason_code = "codex-permission-denied", message = "denied by policy" }}
"#
    )
}

#[test]
fn advise_is_rejected_where_the_bound_event_cannot_render_context() {
    // Codex PermissionRequest supports block but not context, so an advise
    // projection there would be a silent allow.
    let declared = Fixture::new(&codex_block_policy("advise"));
    let output = declared.run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "rule-override-advise-unsupported"
    );

    let overridden = Fixture::new(&codex_block_policy("enforce"));
    add_override(&overridden, RULE_ID, "advise");
    let output = overridden.run(&["validate", "--format", "json"], None);
    assert_eq!(output.code, 65, "stdout={}", output.stdout_text());
    assert_eq!(
        output.stdout_json()["error"]["code"],
        "rule-override-advise-unsupported"
    );

    let inventory = Fixture::new(&codex_block_policy("enforce"));
    let output = inventory.run(&["inventory", "--format", "json"], None);
    assert_eq!(output.code, 0);
    let rule = output.stdout_json()["data"]["rules"][0].clone();
    assert_eq!(rule["tier"], Value::Null, "non-DSH rules carry no tier");
    assert_eq!(rule["enforcement_default"], Value::Null);
}

#[test]
fn a_repeated_advisory_is_allowed_within_one_session_and_reset_across_sessions() {
    let fixture = Fixture::new(&policy_for("block-direct-python", "locked"));
    fs::write(fixture.root.join("uv.lock"), "version = 1\n").expect("uv marker");
    let (_, first) = dispatch(&fixture, "dsh-session-1", "python -c 'print(1)'");
    assert_eq!(first["data"]["action"], "context", "first advisory renders");

    let (code, second) = dispatch(&fixture, "dsh-session-1", "python -c 'print(2)'");
    assert_eq!(code, 0);
    assert_eq!(
        second["data"]["action"], "allow",
        "same session, same advisory"
    );
    assert_eq!(
        second["data"]["reasons"][0]["code"],
        "block-direct-python:advisory-repeated"
    );
    assert_eq!(second["data"]["reasons"][0]["disposition"], "allow");

    let (_, other) = dispatch(&fixture, "dsh-session-2", "python -c 'print(1)'");
    assert_eq!(
        other["data"]["action"], "context",
        "another session sees it once"
    );

    let advised = Fixture::new(&policy_for("block-direct-git-commit", "downgrade-only"));
    add_override(&advised, RULE_ID, "advise");
    let (_, first) = dispatch(&advised, "dsh-session-1", "git commit -m one");
    assert_eq!(first["data"]["action"], "context");
    let (_, second) = dispatch(&advised, "dsh-session-1", "git commit -m two");
    assert_eq!(
        second["data"]["action"], "allow",
        "the advise projection dedupes too"
    );
    assert_eq!(second["data"]["enforcement"], Value::Null);
    assert!(second["data"].get("downgraded_by").is_none());
}

#[test]
fn dedupe_fails_open_without_a_private_state_home() {
    let fixture = Fixture::new(&policy_for("block-direct-python", "locked"));
    fs::write(fixture.root.join("uv.lock"), "version = 1\n").expect("uv marker");
    let state_home = fixture.state_home.join("dsh-runtime-kit");
    fs::create_dir_all(&state_home).expect("state home");
    fs::write(state_home.join("agent-hook"), "not a directory").expect("collision");
    for _ in 0..2 {
        let (_, envelope) = dispatch(&fixture, "dsh-session-1", "python -c 'print(1)'");
        assert_eq!(
            envelope["data"]["action"], "context",
            "without dedupe state the reminder keeps rendering"
        );
        assert_eq!(
            envelope["data"]["reasons"][0]["code"],
            "block-direct-python"
        );
    }

    // A symlinked per-session marker directory is never trusted either.
    let linked = Fixture::new(&policy_for("block-direct-python", "locked"));
    fs::write(linked.root.join("uv.lock"), "version = 1\n").expect("uv marker");
    let dedupe_root = linked
        .state_home
        .join("dsh-runtime-kit/agent-hook/dsh-advisory-dedupe");
    let (_, first) = dispatch(&linked, "dsh-session-1", "python -c 'print(1)'");
    assert_eq!(first["data"]["action"], "context", "{first}");
    let session_dir = fs::read_dir(&dedupe_root)
        .expect("dedupe root")
        .map(|entry| entry.expect("entry").path())
        .next()
        .expect("one per-session marker directory");
    fs::remove_dir_all(&session_dir).expect("drop the real directory");
    let elsewhere = linked.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("target dir");
    std::os::unix::fs::symlink(&elsewhere, &session_dir).expect("symlink");
    for _ in 0..2 {
        let (_, envelope) = dispatch(&linked, "dsh-session-1", "python -c 'print(1)'");
        assert_eq!(envelope["data"]["action"], "context", "{envelope}");
        assert_eq!(
            envelope["data"]["reasons"][0]["code"],
            "block-direct-python"
        );
    }
    assert_eq!(
        fs::read_dir(&elsewhere).expect("target dir").count(),
        0,
        "nothing is written through the symlink"
    );
}
