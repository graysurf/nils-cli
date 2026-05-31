//! Typed payload helpers built on the existing [`crate::lifecycle_record`]
//! data shapes.
//!
//! Task 1.1 keeps the existing payload definitions (`RecordPayload`,
//! `StateData`, `SessionData`, `ValidationData`, `ReviewData`,
//! `CloseoutData`, `SnapshotData`) as the canonical structs. As the vNext
//! registry, lint, and render modules mature, this module is the boundary
//! that hides the catch-all file from new consumers.
//!
//! Re-exports surface only the types that vNext code needs so we can move
//! the underlying definitions during Task 6.3 (migrate record rendering
//! internals to the vNext registry) without rewriting downstream imports.

pub use crate::lifecycle_record::{
    CloseoutData, PayloadProfile, PayloadRole, PrLifecycleStatus, PrRefPayload, RecordPayload,
    ReviewData, ReviewDecision, SessionData, SnapshotData, StateData, StateStatus, TaskRowPayload,
    TaskRowStatus, ValidationCommand, ValidationCommandStatus, ValidationData, ValidationOverall,
    ValidationWaiver,
};
