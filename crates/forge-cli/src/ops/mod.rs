//! Per-op handlers. Each op lives in its own module and exposes a `run`
//! function that consumes the parsed CLI plus the resolved `OutputFormat` and
//! returns `Result<i32, ForgeError>`.

pub mod auth_status;
pub mod repo_view;
