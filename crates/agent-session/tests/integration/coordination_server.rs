use std::path::Path;

use nils_test_support::cmd::{CmdOptions, CmdOutput, run_resolved};
use pretty_assertions::assert_eq;

fn run(dir: &Path, args: &[&str]) -> CmdOutput {
    run_resolved("agent-session", args, &CmdOptions::new().with_cwd(dir))
}

#[test]
fn coordination_server_help_declares_versioned_work_context_and_mailbox_routes() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let output = run(tmp.path(), &["serve", "--help"]);

    assert_eq!(output.code, 0, "stderr={}", output.stderr_text());
    let help = output.stdout_text();
    assert!(help.contains("work-context/v1"), "{help}");
    assert!(help.contains("messages/v1"), "{help}");
    assert!(help.contains("loopback"), "{help}");
}
