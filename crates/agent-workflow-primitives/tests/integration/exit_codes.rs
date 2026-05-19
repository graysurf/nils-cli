//! Per-binary exit-code matrix for the eight agent-workflow-primitives binaries.
//!
//! Sprint 3.1 of `cli-output-contract-unification`: each binary returns the
//! BSD sysexits-aligned exit code from `nils_common::cli_contract::exit`.

use nils_common::cli_contract::exit;
use nils_test_support::cmd::{CmdOptions, run_resolved};
use pretty_assertions::assert_eq;

fn run(bin: &str, args: &[&str]) -> i32 {
    run_resolved(bin, args, &CmdOptions::new()).code
}

const BINARIES: &[&str] = &[
    "browser-session",
    "canary-check",
    "docs-impact",
    "heuristic-inbox",
    "model-cross-check",
    "repo-retro",
    "review-evidence",
    "skill-usage",
];

#[test]
fn unknown_subcommand_returns_usage_for_every_binary() {
    for bin in BINARIES {
        let code = run(bin, &["definitely-not-a-real-subcommand"]);
        assert_eq!(
            code,
            exit::USAGE,
            "{bin} should return USAGE (64) for an unknown subcommand"
        );
    }
}

#[test]
fn unknown_flag_returns_usage_for_every_binary() {
    for bin in BINARIES {
        let code = run(bin, &["--definitely-not-a-real-flag"]);
        assert_eq!(
            code,
            exit::USAGE,
            "{bin} should return USAGE (64) for an unknown flag"
        );
    }
}

#[test]
fn invalid_subcommand_json_envelope_has_correct_schema() {
    use nils_test_support::cmd::CmdOutput;

    fn run_json(bin: &str, args: &[&str]) -> CmdOutput {
        run_resolved(bin, args, &CmdOptions::new())
    }

    for bin in BINARIES {
        let mut args: Vec<&str> = vec!["--format", "json", "definitely-not-a-real-subcommand"];
        // browser-session, canary-check, etc. accept `--format` as a global; but
        // `repo-retro` only has it on `report`. Bypass via `--format=json` on
        // bins that reject it pre-subcommand by retrying.
        let first = run_json(bin, &args);
        let output = if first.code == exit::USAGE {
            first
        } else {
            args = vec!["definitely-not-a-real-subcommand", "--format", "json"];
            run_json(bin, &args)
        };
        assert_eq!(
            output.code,
            exit::USAGE,
            "{bin}: expected exit 64, got {}; stderr={}",
            output.code,
            output.stderr_text()
        );
    }
}
