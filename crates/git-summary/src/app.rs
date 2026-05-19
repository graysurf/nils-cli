use std::env;

use nils_common::cli_contract::exit;

use crate::cli::{print_header, print_help};
use crate::dates::{
    last_month_range, last_week_range, this_month_range, this_week_range, today_range,
    yesterday_range,
};
use crate::git::require_git;
use crate::summary::summary;

pub fn run() -> i32 {
    let args: Vec<String> = env::args().skip(1).collect();

    if let Some(code) = crate::completion::maybe_handle_completion_export(&args) {
        return code;
    }

    if args.is_empty() || is_help(&args[0]) {
        print_help();
        return exit::SUCCESS;
    }
    if is_version(&args[0]) {
        println!("git-summary {}", env!("CARGO_PKG_VERSION"));
        return exit::SUCCESS;
    }

    let cmd = args[0].as_str();
    match cmd {
        "all" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            print_header("all commits");
            summary(None, None)
        }
        "today" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = today_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        "yesterday" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = yesterday_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        "this-month" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = this_month_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        "last-month" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = last_month_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        "this-week" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = this_week_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        "last-week" => {
            if let Err(msg) = require_git() {
                println!("{msg}");
                return exit::RUNTIME;
            }
            let range = last_week_range();
            print_header(&range.label);
            summary(Some(&range.start), Some(&range.end))
        }
        _ => {
            if args.len() >= 2 {
                if let Err(msg) = require_git() {
                    println!("{msg}");
                    return exit::RUNTIME;
                }
                summary(Some(&args[0]), Some(&args[1]))
            } else {
                println!("❌ Invalid usage. Try: git-summary help");
                exit::USAGE
            }
        }
    }
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "help" | "--help" | "-h")
}

fn is_version(arg: &str) -> bool {
    matches!(arg, "--version" | "-V")
}

#[cfg(test)]
mod tests {
    use super::{is_help, is_version};

    #[test]
    fn help_token_detection_matches_supported_aliases() {
        assert!(is_help("help"));
        assert!(is_help("--help"));
        assert!(is_help("-h"));
        assert!(!is_help("today"));
    }

    #[test]
    fn version_token_detection_matches_supported_aliases() {
        assert!(is_version("--version"));
        assert!(is_version("-V"));
        assert!(!is_version("today"));
    }
}
