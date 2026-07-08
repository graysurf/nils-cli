//! Per-op handlers. Each op lives in its own module and exposes a `run`
//! function that consumes the parsed CLI plus the resolved `OutputFormat` and
//! returns `Result<i32, ForgeError>`.

pub mod activity;
pub mod auth_status;
pub mod gitlab_api;
pub mod inbox;
pub mod issue_close;
pub mod issue_closeout;
pub mod issue_comment;
pub mod issue_create;
pub mod issue_edit;
pub mod issue_list;
pub mod issue_reopen;
pub mod issue_view;
pub mod label;
pub mod pr_checks;
pub mod pr_checks_gitlab;
pub mod pr_close;
pub mod pr_comment;
pub mod pr_comments;
pub mod pr_create;
pub mod pr_edit;
pub mod pr_list;
pub mod pr_merge;
pub mod pr_ready;
pub mod pr_review;
pub mod pr_review_thread_reply;
pub mod pr_review_thread_resolve;
pub mod pr_review_threads;
pub mod pr_state;
pub mod pr_tasks;
pub mod pr_view;
pub mod pr_wait_checks;
pub mod repo_view;
pub mod required_check_gate;
pub mod search;
