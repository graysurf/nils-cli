// Consolidated integration test target.

#[path = "integration/agent_run.rs"]
mod agent_run;
#[path = "integration/agent_run_inspect.rs"]
mod agent_run_inspect;
#[path = "integration/agent_run_inspect_unavailable.rs"]
mod agent_run_inspect_unavailable;
#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/completion_export.rs"]
mod completion_export;
#[path = "integration/control_plane.rs"]
mod control_plane;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/review_specialists.rs"]
mod review_specialists;
#[path = "integration/test_first_evidence.rs"]
mod test_first_evidence;
