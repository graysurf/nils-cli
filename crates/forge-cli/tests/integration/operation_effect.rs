use super::support::{StubEnv, parse_envelope, run_forge_cli};

#[test]
fn issue_view_emits_a_bound_network_read_descriptor() {
    let stub = StubEnv::new();
    let output = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "issue",
            "view",
            "670",
            "--repo",
            "graysurf/agent-runtime-kit",
            "--format",
            "json",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    let json = parse_envelope(&output.stdout);
    assert_eq!(json["schema_version"], "cli.forge-cli.operation-effect.v1");
    assert_eq!(json["ok"], true);
    assert_eq!(
        json["data"]["schema_version"],
        "execution.operation-effect.v1"
    );
    assert_eq!(json["data"]["producer"]["tool"], "forge-cli");
    assert_eq!(
        json["data"]["producer"]["release"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(json["data"]["capability_class"], "tool_contract");
    assert_eq!(json["data"]["effect"], "read_only");
    assert_eq!(json["data"]["operation"], "issue.view");
    assert_eq!(json["data"]["provider_effect"], "network_read");
}

#[test]
fn issue_edit_is_never_described_as_read_only() {
    let stub = StubEnv::new();
    let output = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "issue",
            "edit",
            "670",
            "--title",
            "changed",
            "--repo",
            "graysurf/agent-runtime-kit",
        ],
    );

    assert_eq!(output.code, 0, "stderr={}", output.stderr);
    assert_ne!(
        parse_envelope(&output.stdout)["data"]["effect"],
        "read_only"
    );
}

#[test]
fn unknown_inner_flags_produce_no_descriptor() {
    let stub = StubEnv::new();
    let output = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "issue",
            "view",
            "670",
            "--unknown-effect-flag",
        ],
    );

    assert_eq!(output.code, 64);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.contains("unexpected argument"));
}

#[test]
fn inbox_requires_explicit_no_cache_to_avoid_managed_state_writes() {
    let stub = StubEnv::new();
    let cached = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "inbox",
            "list",
        ],
    );
    let uncached = run_forge_cli(
        &stub,
        &[
            "operation-effect",
            "--format",
            "json",
            "--",
            "inbox",
            "list",
            "--no-cache",
        ],
    );

    assert_eq!(cached.code, 0, "stderr={}", cached.stderr);
    assert_ne!(
        parse_envelope(&cached.stdout)["data"]["effect"],
        "read_only"
    );
    assert_eq!(uncached.code, 0, "stderr={}", uncached.stderr);
    assert_eq!(
        parse_envelope(&uncached.stdout)["data"]["effect"],
        "read_only"
    );
}
