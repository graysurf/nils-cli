//! Crate-local re-export of the shared JSON envelope helpers in
//! `nils_common::diag_output`. Kept as a stable callsite alias so existing
//! `crate::diag_output::*` paths continue to work after the consolidation.

pub use nils_common::diag_output::*;
