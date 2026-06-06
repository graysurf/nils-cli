pub mod cli;
pub mod completion;

mod runtime;

pub fn run() -> i32 {
    cli::run()
}
