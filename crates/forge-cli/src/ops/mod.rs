//! Per-op handlers. Each op lives in its own module and exposes a `run`
//! function that consumes the parsed CLI plus the resolved `OutputFormat` and
//! returns `Result<i32, ForgeError>`.

pub mod auth_status;
pub mod pr_checks;
pub mod pr_checks_gitlab;
pub mod pr_close;
pub mod pr_comment;
pub mod pr_create;
pub mod pr_edit;
pub mod pr_list;
pub mod pr_ready;
pub mod pr_state;
pub mod pr_view;
pub mod pr_wait_checks;
pub mod repo_view;
pub mod required_check_gate;
