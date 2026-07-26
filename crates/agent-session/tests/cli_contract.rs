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
        "cancel",
        "retire",
    ] {
        assert!(
            worker.contains(command),
            "worker help omitted {command}: {worker}"
        );
    }

    let root = help(&["--help"]);
    for contract in [
        "MACRO-FIRST RECOVERY",
        "last_proven_safe_state",
        "never resend a prompt",
        "unbounded/manual Enter",
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
}

#[test]
fn worker_start_is_readiness_first_by_default_with_explicit_launch_only_opt_out() {
    let start = help(&["worker", "start", "--help"]).to_ascii_lowercase();
    assert!(start.contains("default: 5m"), "worker start help: {start}");
    assert!(
        start.contains("0 = launch-only"),
        "worker start help: {start}"
    );
}
