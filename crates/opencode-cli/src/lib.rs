#![forbid(unsafe_code)]

pub mod agent;
mod cli;
mod completion;
pub mod paths;
pub mod prompts;

pub fn run() -> i32 {
    cli::run_from(std::env::args_os())
}
