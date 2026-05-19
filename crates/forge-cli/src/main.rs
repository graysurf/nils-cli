use std::process;

fn main() {
    let exit_code = forge_cli::run(std::env::args_os().skip(1).collect());
    process::exit(exit_code);
}
