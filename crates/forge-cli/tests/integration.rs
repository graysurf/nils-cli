//! Top-level integration test harness. Individual test modules live under
//! `tests/integration/`.

mod integration {
    mod activity;
    mod auth_status;
    mod cli;
    mod completion_sync;
    mod conformance;
    mod exit_codes;
    mod exit_codes_full;
    mod fixture_lint;
    mod inbox;
    mod issue_atoms;
    mod label_ops;
    mod local_ops;
    mod local_path_guard;
    mod parity;
    mod pr_checks_github;
    mod pr_checks_gitlab;
    mod pr_create;
    mod pr_deliver;
    mod pr_deliver_chain;
    mod pr_merge;
    mod pr_review;
    mod pr_wait_checks;
    mod repo_view;
    mod required_check_gate;
    mod search;
    mod support;
    mod validations;
}
