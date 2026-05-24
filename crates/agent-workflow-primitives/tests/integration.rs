// Consolidated integration test target.

#[path = "integration/agent_run.rs"]
mod agent_run;
#[path = "integration/cli.rs"]
mod cli;
#[path = "integration/exit_codes.rs"]
mod exit_codes;
#[path = "integration/help_snapshot.rs"]
mod help_snapshot;
#[path = "integration/review_specialists.rs"]
mod review_specialists;
#[path = "integration/test_first_evidence.rs"]
mod test_first_evidence;
