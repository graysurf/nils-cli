mod artifact_audit;
mod batches;
mod bundle;
mod completion;
mod fix;
pub mod parse;
mod repo_root;
mod repr;
mod scaffold;
mod spec;
pub mod split_prs;
mod usage;
mod validate;

pub fn run() -> i32 {
    let args: Vec<String> = std::env::args().collect();
    usage::dispatch(&args)
}
