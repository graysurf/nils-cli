mod cli;
mod completion;
mod dates;
mod git;
mod summary;

fn main() {
    std::process::exit(cli::run());
}
