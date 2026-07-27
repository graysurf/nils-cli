use std::process::Command;

use nils_test_support::bin;

fn help(args: &[&str]) -> String {
    let output = Command::new(bin::resolve("main-agent"))
        .args(args)
        .output()
        .expect("run main-agent");
    assert!(
        output.status.success(),
        "args={args:?} stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 help")
}

#[test]
fn macro_first_worker_surface_is_discoverable() {
    let worker = help(&["worker", "--help"]);
    for command in [
        "supervise",
        "reassign",
        "diagnose",
        "submit-recovery",
        "reconcile-recovery",
        "account-handoff-cancel",
        "request-changes",
        "cancel",
        "retire",
    ] {
        assert!(
            worker.contains(command),
            "worker help omitted {command}: {worker}"
        );
    }

    let root = help(&["--help"]).to_ascii_lowercase();
    for contract in [
        "macro-first recovery",
        "last_proven_safe_state",
        "never resend a prompt",
        "unbounded/manual enter",
        "current assignment revision",
        "--authorize-account-change",
    ] {
        assert!(
            root.contains(contract),
            "root help omitted {contract}: {root}"
        );
    }
}

#[test]
fn recovery_primitives_publish_their_guards() {
    let submit = help(&["worker", "submit-recovery", "--help"]).to_ascii_lowercase();
    assert!(submit.contains("single guarded enter"));
    assert!(submit.contains("1-30s"));
    assert!(submit.contains("expected current assignment revision"));

    let reconcile = help(&["worker", "reconcile-recovery", "--help"]).to_ascii_lowercase();
    assert!(reconcile.contains("without sending input"));
    assert!(reconcile.contains("stopped"));
    assert!(reconcile.contains("quiescent"));
    assert!(reconcile.contains("expected current assignment revision"));

    let cancel = help(&["worker", "cancel", "--help"]).to_ascii_lowercase();
    assert!(cancel.contains("failed pre-claim assignment"));
    assert!(cancel.contains("expected current assignment revision"));

    let reassign = help(&["worker", "reassign", "--help"]).to_ascii_lowercase();
    assert!(reassign.contains("distinct replacement assignment"));
    assert!(reassign.contains("clean worktree"));
    assert!(reassign.contains("without reusing its prompt or worktree"));

    let request_changes = help(&["worker", "request-changes", "--help"]).to_ascii_lowercase();
    assert!(request_changes.contains("return a submitted assignment"));
    assert!(request_changes.contains("expected current assignment revision"));
    assert!(request_changes.contains("bounded durable reason"));
}

#[test]
fn worker_start_preserves_bounded_readiness_by_default_with_launch_only_opt_out() {
    let start = help(&["worker", "start", "--help"]).to_ascii_lowercase();
    assert!(start.contains("default: 5m"), "worker start help: {start}");
    assert!(
        start.contains("0 = launch-only"),
        "worker start help: {start}"
    );
    for (name, docs) in [
        ("README", include_str!("../README.md")),
        (
            "orchestration runbook",
            include_str!("../docs/runbooks/main-agent-orchestration.md"),
        ),
    ] {
        assert!(
            docs.contains("defaults to waiting up to 5 minutes"),
            "{name} must publish the same omitted readiness default as CLI help"
        );
        assert!(
            docs.contains("`--await-ready 0`"),
            "{name} must publish the explicit launch-only opt-out"
        );
    }
}

#[test]
fn completions_publish_account_handoff_cancellation_guards() {
    for shell in ["bash", "zsh"] {
        let completion = help(&["completion", shell]);
        for contract in [
            "account-handoff-cancel",
            "request-changes",
            "--if-revision",
            "--reason",
            "--authorize-account-change",
        ] {
            assert!(
                completion.contains(contract),
                "{shell} completion omitted {contract}"
            );
        }
    }
}
