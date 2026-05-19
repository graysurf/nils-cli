mod cli;
mod completion;
mod confirm;
mod defs;
mod directory;
mod file;
mod fzf;
mod git_branch;
mod git_checkout;
mod git_commit;
mod git_commit_select;
mod git_status;
mod git_tag;
mod history;
mod kill;
mod open;
mod port;
mod process;
mod util;

fn main() {
    std::process::exit(cli::run());
}
