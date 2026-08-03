//! Top-level integration test harness. Individual test modules live under
//! `tests/integration/`.

mod integration {
    mod activity;
    mod agent_attribution_guard;
    mod auth_status;
    mod cli;
    mod completion_sync;
    mod conformance;
    mod exit_codes;
    mod exit_codes_full;
    mod fixture_lint;
    mod forgejo_http;
    mod inbox;
    mod issue_atoms;
    mod label_ops;
    mod ledger_blank_comment_probe;
    mod local_ops;
    mod local_path_guard;
    mod operation_effect;
    mod parity;
    mod pr_checks_github;
    mod pr_checks_gitlab;
    mod pr_create;
    mod pr_deliver;
    mod pr_deliver_chain;
    mod pr_merge;
    mod pr_pending_review;
    mod pr_review;
    mod pr_review_loop;
    mod pr_reviews;
    mod pr_wait_checks;
    mod provider_registry;
    mod rate_limit_gate;
    mod repo_bootstrap;
    mod repo_push_default;
    mod repo_view;
    mod required_check_gate;
    mod search;
    mod support;
    mod validations;
}
