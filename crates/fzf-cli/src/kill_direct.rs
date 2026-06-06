use std::process::Output;

use nils_common::cli_contract::exit;

use crate::{kill, util};

pub fn run_kill_process(args: &[String]) -> i32 {
    let parsed = match parse_direct_args(args, "fzf-cli kill-process [-9|--force] <pid> [pid...]") {
        DirectParse::Run(parsed) => parsed,
        DirectParse::Exit(code) => return code,
    };

    let pids = match validate_pids(&parsed.operands) {
        Ok(pids) => pids,
        Err(message) => {
            eprintln!("{message}");
            return exit::USAGE;
        }
    };

    run_kill_flow(&pids, parsed.force)
}

pub fn run_kill_port(args: &[String]) -> i32 {
    let parsed = match parse_direct_args(args, "fzf-cli kill-port [-9|--force] <port> [port...]") {
        DirectParse::Run(parsed) => parsed,
        DirectParse::Exit(code) => return code,
    };

    let ports = match validate_ports(&parsed.operands) {
        Ok(ports) => ports,
        Err(message) => {
            eprintln!("{message}");
            return exit::USAGE;
        }
    };

    if !util::cmd_exists("lsof") {
        eprintln!("fzf-cli kill-port: lsof not found");
        return exit::RUNTIME;
    }

    let mut pids = Vec::new();
    for port in &ports {
        match pids_for_port(port) {
            Ok(mut found) => pids.append(&mut found),
            Err(message) => {
                eprintln!("{message}");
                return exit::RUNTIME;
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();

    if pids.is_empty() {
        println!("ℹ️  No process found on port(s): {}", ports.join(" "));
        return exit::SUCCESS;
    }

    run_kill_flow(&pids, parsed.force)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectArgs {
    force: bool,
    operands: Vec<String>,
}

enum DirectParse {
    Run(DirectArgs),
    Exit(i32),
}

fn parse_direct_args(args: &[String], usage: &str) -> DirectParse {
    let mut force = false;
    let mut operands = Vec::new();
    let mut parsing_operands = false;

    for arg in args {
        if !parsing_operands {
            match arg.as_str() {
                "-h" | "--help" | "help" => {
                    println!("Usage: {usage}");
                    return DirectParse::Exit(exit::SUCCESS);
                }
                "-9" | "--force" => {
                    force = true;
                    continue;
                }
                "--" => {
                    parsing_operands = true;
                    continue;
                }
                _ if arg.starts_with('-') => {
                    eprintln!("fzf-cli: error: unknown flag for direct kill command: {arg}");
                    eprintln!("Usage: {usage}");
                    return DirectParse::Exit(exit::USAGE);
                }
                _ => {
                    parsing_operands = true;
                }
            }
        }

        operands.push(arg.clone());
    }

    if operands.is_empty() {
        eprintln!("Usage: {usage}");
        return DirectParse::Exit(exit::USAGE);
    }

    DirectParse::Run(DirectArgs { force, operands })
}

fn validate_pids(raw: &[String]) -> Result<Vec<String>, String> {
    let mut pids = Vec::with_capacity(raw.len());
    for value in raw {
        if !is_positive_decimal(value) {
            return Err(format!("fzf-cli kill-process: invalid pid: {value}"));
        }
        pids.push(value.clone());
    }
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn validate_ports(raw: &[String]) -> Result<Vec<String>, String> {
    let mut ports = Vec::with_capacity(raw.len());
    for value in raw {
        if !is_positive_decimal(value) {
            return Err(format!("fzf-cli kill-port: invalid port: {value}"));
        }
        let port = value
            .parse::<u32>()
            .map_err(|_| format!("fzf-cli kill-port: invalid port: {value}"))?;
        if port > 65_535 {
            return Err(format!("fzf-cli kill-port: port out of range: {value}"));
        }
        ports.push(value.clone());
    }
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn is_positive_decimal(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()) && value != "0"
}

fn pids_for_port(port: &str) -> Result<Vec<String>, String> {
    let mut pids = Vec::new();
    pids.extend(lsof_pids(&[
        "-nP",
        &format!("-iTCP:{port}"),
        "-sTCP:LISTEN",
        "-t",
    ])?);
    pids.extend(lsof_pids(&["-nP", &format!("-iUDP:{port}"), "-t"])?);
    pids.sort_unstable();
    pids.dedup();
    Ok(pids)
}

fn lsof_pids(args: &[&str]) -> Result<Vec<String>, String> {
    let output = util::run_output("lsof", args).map_err(|err| format!("{err:#}"))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(parse_lsof_pid_output(&output))
}

fn parse_lsof_pid_output(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| is_positive_decimal(line))
        .map(ToOwned::to_owned)
        .collect()
}

fn run_kill_flow(pids: &[String], force: bool) -> i32 {
    match kill::kill_flow(pids, true, force) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{err:#}");
            exit::RUNTIME
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_direct_args_accepts_force_and_operands() {
        let args = vec!["-9".to_string(), "123".to_string(), "456".to_string()];
        let DirectParse::Run(parsed) = parse_direct_args(&args, "usage") else {
            panic!("expected run");
        };
        assert!(parsed.force);
        assert_eq!(parsed.operands, vec!["123".to_string(), "456".to_string()]);
    }

    #[test]
    fn validate_pids_rejects_zero_and_non_numeric_values() {
        assert!(validate_pids(&["123".to_string()]).is_ok());
        assert!(validate_pids(&["0".to_string()]).is_err());
        assert!(validate_pids(&["abc".to_string()]).is_err());
    }

    #[test]
    fn validate_ports_rejects_out_of_range_values() {
        assert!(validate_ports(&["3000".to_string()]).is_ok());
        assert!(validate_ports(&["65536".to_string()]).is_err());
        assert!(validate_ports(&["0".to_string()]).is_err());
    }
}
