use crate::purge_state::{self, Confirm, PurgeError, Scope};
use clap::Args;
use std::io::{BufReader, stderr, stdin, stdout};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct PurgeStateArgs {
    /// Absolute path of the state home (the directory whose `out/`
    /// and `backups/` subtrees are subject to purge).
    #[arg(long)]
    pub state_home: PathBuf,
    /// Required scope: `out` clears `<state_home>/out/`; `backups`
    /// clears `<state_home>/backups/`; `all` clears both. No default.
    #[arg(long)]
    pub scope: Option<String>,
    /// Skip the interactive confirmation prompt and log a single
    /// `--yes` audit line to stderr. Reserved for CI / scripted
    /// contexts; interactive operators should answer the prompt.
    #[arg(long, default_value_t = false)]
    pub yes: bool,
}

pub fn run(args: PurgeStateArgs) -> anyhow::Result<u8> {
    if !args.state_home.is_absolute() {
        anyhow::bail!(
            "agent-runtime purge-state: --state-home must be absolute (got: {})",
            args.state_home.display()
        );
    }
    let scope_str = match args.scope.as_deref() {
        Some(s) => s,
        None => {
            anyhow::bail!(
                "agent-runtime purge-state: --scope is required (one of `out`, `backups`, `all`)"
            );
        }
    };
    let scope: Scope = scope_str
        .parse()
        .map_err(|err: String| anyhow::anyhow!(err))?;

    let outcome = if args.yes {
        purge_state::run(&args.state_home, scope, Confirm::Yes, &mut stderr())
    } else {
        let stdin_h = stdin();
        let mut reader = BufReader::new(stdin_h.lock());
        let stdout_h = stdout();
        let mut writer = stdout_h.lock();
        let mut audit = stderr();
        purge_state::run(
            &args.state_home,
            scope,
            Confirm::Prompt {
                reader: &mut reader,
                writer: &mut writer,
            },
            &mut audit,
        )
    };

    let outcome = match outcome {
        Ok(o) => o,
        Err(PurgeError::Cancelled) => {
            eprintln!("agent-runtime purge-state: cancelled");
            return Ok(1);
        }
        Err(err) => return Err(err.into()),
    };

    eprintln!(
        "agent-runtime purge-state: scope={} cleared={}",
        outcome.scope.as_str(),
        outcome.cleared.len()
    );
    for path in &outcome.cleared {
        eprintln!("  - cleared {}", path.display());
    }
    Ok(0)
}
