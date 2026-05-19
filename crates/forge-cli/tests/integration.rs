//! Top-level integration test harness. Individual test modules live under
//! `tests/integration/`.

mod integration {
    mod auth_status;
    mod cli;
    mod exit_codes;
    mod pr_create;
    mod repo_view;
    mod support;
    mod validations;
}
