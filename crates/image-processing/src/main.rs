use clap::{CommandFactory, Parser};
use nils_common::cli_contract::exit;
use nils_term::progress::{Progress, ProgressFinish, ProgressOptions};
use std::path::PathBuf;
use std::process;

mod cli;
mod completion;
mod convert;
mod model;
mod processing;
mod report;
mod svg_validate;
mod toolchain;
mod util;

use cli::{Cli, Operation};

fn main() {
    process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = handle_completion_export(&args) {
        return code;
    }

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(err) => {
            use clap::error::ErrorKind;
            let code = match err.kind() {
                ErrorKind::DisplayHelp
                | ErrorKind::DisplayVersion
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => err.exit_code(),
                _ => exit::USAGE,
            };
            let _ = err.print();
            return code;
        }
    };

    if let Err(e) = validate(&cli) {
        usage_error(&e.message);
    }

    let repo_root = util::find_repo_root();

    let inputs = match processing::expand_inputs(&cli.inputs) {
        Ok(v) => v,
        Err(e) => usage_error(&e.message),
    };
    let input_path = inputs.into_iter().next().expect("validated single input");

    let output_path = util::expand_user(cli.out.as_deref().expect("validated output path"));

    let mut run_dir: Option<PathBuf> = None;
    if cli.json || cli.report {
        let run_id = util::now_run_id();
        let path = repo_root
            .join("out")
            .join("image-processing")
            .join("runs")
            .join(run_id);
        if let Err(err) = std::fs::create_dir_all(&path) {
            eprintln!("image-processing: error: {err}");
            return 1;
        }
        run_dir = Some(path);
    }

    let progress = Progress::new(
        1,
        ProgressOptions::default().with_finish(ProgressFinish::Leave),
    );

    let summary = match processing::process_items(processing::ProcessArgs {
        repo_root: &repo_root,
        run_dir: run_dir.as_deref(),
        progress,
        subcommand: cli.subcommand,
        input_path: &input_path,
        output_path: &output_path,
        convert_to: cli.to.as_deref(),
        width: if cli.subcommand == Operation::Convert {
            cli.width
        } else {
            None
        },
        height: if cli.subcommand == Operation::Convert {
            cli.height
        } else {
            None
        },
        overwrite: cli.overwrite,
        dry_run: cli.dry_run,
        report_enabled: cli.report,
        json_enabled: cli.json,
    }) {
        Ok(summary) => summary,
        Err(err) => {
            if err.downcast_ref::<util::UsageError>().is_some() {
                usage_error(&err.to_string());
            }
            eprintln!("image-processing: error: {err}");
            return 1;
        }
    };

    if cli.json {
        match serde_json::to_string(&summary) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!("image-processing: error: {err}");
                return 1;
            }
        }
    } else {
        println!("operation: {}", summary.operation);
        if let Some(path) = run_dir.as_deref() {
            println!("run_dir: {}", util::maybe_relpath(path, &repo_root));
        }
        for item in &summary.items {
            let output = item.output_path.as_deref().unwrap_or("None");
            println!("- {}: {} -> {}", item.status, item.input_path, output);
        }
    }

    if summary.items.iter().any(|item| item.status == "error") {
        1
    } else {
        0
    }
}

fn handle_completion_export(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("completion") {
        return None;
    }

    match args.get(1).map(String::as_str) {
        Some("-h") | Some("--help") => {
            println!("usage: image-processing completion <bash|zsh>");
            Some(0)
        }
        Some("bash") if args.len() == 2 => Some(completion::run(completion::CompletionShell::Bash)),
        Some("zsh") if args.len() == 2 => Some(completion::run(completion::CompletionShell::Zsh)),
        Some(shell) if args.len() == 2 => {
            eprintln!("image-processing: error: unsupported completion shell '{shell}'");
            eprintln!("usage: image-processing completion <bash|zsh>");
            Some(64)
        }
        _ => {
            eprintln!("image-processing: error: expected `image-processing completion <bash|zsh>`");
            Some(64)
        }
    }
}

fn validate(cli: &Cli) -> Result<(), util::UsageError> {
    let forbid = |flag: &str| -> Result<(), util::UsageError> {
        Err(util::UsageError {
            message: format!("{} does not support {flag}", cli.subcommand.as_str()),
        })
    };

    match cli.subcommand {
        Operation::Convert => {
            if cli.inputs.len() != 1 {
                return Err(util::UsageError {
                    message: "convert requires exactly one --in <path>".to_string(),
                });
            }

            if cli.out.is_none() {
                return Err(util::UsageError {
                    message: "convert requires --out".to_string(),
                });
            }

            if cli.to.is_none() {
                return Err(util::UsageError {
                    message: "convert requires --to png|webp|jpg".to_string(),
                });
            }

            if let Some(width) = cli.width
                && width <= 0
            {
                return Err(util::UsageError {
                    message: "convert --width must be > 0".to_string(),
                });
            }

            if let Some(height) = cli.height
                && height <= 0
            {
                return Err(util::UsageError {
                    message: "convert --height must be > 0".to_string(),
                });
            }

            if let Some(to) = cli.to.as_deref()
                && convert::normalize_convert_target(to).is_none()
            {
                return Err(util::UsageError {
                    message: "convert --to must be one of: png|webp|jpg".to_string(),
                });
            }
        }
        Operation::SvgValidate => {
            if cli.inputs.len() != 1 {
                return Err(util::UsageError {
                    message: "svg-validate requires exactly one --in <path>".to_string(),
                });
            }

            if cli.out.is_none() {
                return Err(util::UsageError {
                    message: "svg-validate requires --out".to_string(),
                });
            }

            if cli.to.is_some() {
                forbid("--to")?;
            }
            if cli.width.is_some() {
                forbid("--width")?;
            }
            if cli.height.is_some() {
                forbid("--height")?;
            }
        }
    }

    Ok(())
}

fn usage_error(msg: &str) -> ! {
    let mut cmd = Cli::command();
    let usage = cmd.render_usage().to_string();
    eprintln!("{usage}");
    eprintln!("image-processing: error: {msg}");
    process::exit(exit::USAGE);
}

#[cfg(test)]
mod tests {
    use super::{Cli, Operation, validate};

    fn base_cli(subcommand: Operation) -> Cli {
        Cli {
            subcommand,
            inputs: vec![],
            out: Some(
                match subcommand {
                    Operation::Convert => "out.png",
                    Operation::SvgValidate => "out.svg",
                }
                .to_string(),
            ),
            overwrite: false,
            dry_run: true,
            json: false,
            report: false,
            to: None,
            width: None,
            height: None,
        }
    }

    #[test]
    fn validate_convert_requires_in_and_to() {
        let mut cli = base_cli(Operation::Convert);
        let err = validate(&cli).expect_err("convert requires --in");
        assert!(
            err.to_string()
                .contains("convert requires exactly one --in")
        );

        cli.inputs = vec!["icon.svg".to_string()];
        let err = validate(&cli).expect_err("convert requires --to");
        assert!(
            err.to_string()
                .contains("convert requires --to png|webp|jpg")
        );

        cli.to = Some("png".to_string());
        assert!(validate(&cli).is_ok());
    }

    #[test]
    fn validate_convert_rejects_extra_inputs_and_invalid_dimensions() {
        let mut cli = base_cli(Operation::Convert);
        cli.inputs = vec!["icon.svg".to_string(), "other.png".to_string()];
        cli.to = Some("png".to_string());
        let err = validate(&cli).expect_err("convert should reject multiple --in");
        assert!(err.to_string().contains("exactly one --in"));

        cli.inputs = vec!["icon.svg".to_string()];
        cli.width = Some(0);
        let err = validate(&cli).expect_err("width must be > 0");
        assert!(err.to_string().contains("--width must be > 0"));

        cli.width = Some(64);
        cli.to = Some("gif".to_string());
        let err = validate(&cli).expect_err("gif target should be rejected");
        assert!(err.to_string().contains("png|webp|jpg"));
    }

    #[test]
    fn validate_svg_validate_contract() {
        let mut cli = base_cli(Operation::SvgValidate);
        let err = validate(&cli).expect_err("svg-validate requires one --in");
        assert!(err.to_string().contains("exactly one --in"));

        cli.inputs = vec!["in.svg".to_string()];
        cli.out = None;
        let err = validate(&cli).expect_err("svg-validate requires --out");
        assert!(err.to_string().contains("requires --out"));

        cli.out = Some("out/clean.svg".to_string());
        assert!(validate(&cli).is_ok());
    }

    #[test]
    fn validate_svg_validate_rejects_convert_only_flags() {
        let mut cli = base_cli(Operation::SvgValidate);
        cli.inputs = vec!["in.svg".to_string()];
        cli.to = Some("png".to_string());
        let err = validate(&cli).expect_err("svg-validate should reject --to");
        assert!(
            err.to_string()
                .contains("svg-validate does not support --to")
        );

        cli.to = None;
        cli.width = Some(128);
        let err = validate(&cli).expect_err("svg-validate should reject --width");
        assert!(
            err.to_string()
                .contains("svg-validate does not support --width")
        );
    }
}
