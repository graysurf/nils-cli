//! Top-level integration test harness. Individual test modules live under
//! `tests/integration/`.

mod integration {
    mod auth_status;
    mod cli;
    mod completion_sync;
    mod exit_codes;
    mod exit_codes_full;
    mod fixture_lint;
    mod issue_atoms;
    mod parity;
    mod pr_checks_github;
    mod pr_checks_gitlab;
    mod pr_create;
    mod pr_deliver;
    mod pr_deliver_chain;
    mod pr_merge;
    mod pr_wait_checks;
    mod repo_view;
    mod required_check_gate;
    mod support;
    mod validations;
}
