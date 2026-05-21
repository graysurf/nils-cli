//! Small terminal utilities shared across the workspace.
//!
//! ## Progress
//!
//! `nils-term` provides a minimal, RAII-friendly progress abstraction that is safe for
//! machine-readable stdout output.
//!
//! - Progress is drawn to **stderr** by default.
//! - With `ProgressEnabled::Auto` (default), progress is enabled only when **stderr is a TTY**.
//!
//! ### Determinate progress
//!
//! ```rust
//! use nils_term::progress::{Progress, ProgressOptions};
//!
//! let total = 3_u64;
//! let progress = Progress::new(total, ProgressOptions::default().with_prefix("work "));
//!
//! for i in 0..total {
//!     progress.set_message(format!("item {i}"));
//!     progress.inc(1);
//! }
//!
//! progress.finish();
//! ```
//!
//! ### Spinner progress
//!
//! ```rust
//! use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
//!
//! let spinner = Progress::spinner(
//!     ProgressOptions::default()
//!         .with_prefix("fetch ")
//!         .with_finish(ProgressFinish::Clear),
//! );
//!
//! spinner.set_message("loading");
//! spinner.tick();
//! spinner.finish_and_clear();
//! ```
//!
//! ### Library guidance
//!
//! Prefer accepting a `Progress` (or `ProgressOptions`) from the caller instead of reading env vars
//! inside library code. This keeps libraries deterministic and lets binaries decide whether to show
//! progress (e.g. interactive vs CI).

pub mod progress;
pub mod prompt;
