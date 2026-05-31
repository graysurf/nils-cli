mod artifact_audit;
mod batches;
mod bundle;
mod cli;
mod completion;
pub mod exec_state;
mod fix;
pub mod ledger;
mod ledger_sync;
mod ledger_update;
pub mod parse;
mod repo_root;
mod repr;
mod scaffold;
mod spec;
pub mod split_prs;
mod validate;

pub fn run() -> i32 {
    cli::run()
}
