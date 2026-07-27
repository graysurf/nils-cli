mod cli;
mod commit;
mod completion;
mod default_branch;
mod git;
mod staged_context;

pub fn run() -> i32 {
    cli::run()
}
