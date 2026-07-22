mod cli;
mod commit;
mod completion;
mod git;
mod local_default;
mod staged_context;

pub fn run() -> i32 {
    cli::run()
}
